// SPDX-License-Identifier: MIT OR Apache-2.0
// This file is part of WeaveGate.
// WeaveGate — frontend gateway and static file server.
// Copyright (C) 2019-present Jose Quintana <joseluisq.net>

use hyper::{
    Body, Uri,
    header::{ACCEPT, AUTHORIZATION, USER_AGENT},
};
use std::path::Path;
use std::time::Duration;
use tokio::time::timeout;

use crate::proxy_client::build_bootstrap_client;
use crate::settings::file::{Proxy, RoutesBootstrap, RoutesBootstrapOnFailure};
use crate::settings::ProxyTlsSettings;
use crate::{Context, Result};

use super::document::RoutesDocument;

/// Fetches proxy definitions from the control plane or cache per bootstrap config.
pub async fn fetch_routes_defs(
    config: &RoutesBootstrap,
    weavegate_version: &str,
    tls: &ProxyTlsSettings,
) -> Result<Option<Vec<Proxy>>> {
    match config.on_failure.unwrap_or_default() {
        RoutesBootstrapOnFailure::FileOnly => return Ok(None),
        _ => {}
    }

    let url = &config.url;
    tracing::info!("routes bootstrap: fetching from {}", url);

    let fetch_result = timeout(
        Duration::from_secs(config.timeout_secs.unwrap_or(5)),
        http_fetch_document(config, weavegate_version, tls),
    )
    .await;

    match fetch_result {
        Ok(Ok(doc)) => {
            if let Some(ref cache_path) = config.cache_path {
                if let Err(err) = write_cache(cache_path, &doc) {
                    tracing::warn!("routes bootstrap: failed to write cache: {err:#}");
                }
            }
            let defs = doc.into_proxy_defs()?;
            tracing::info!("routes bootstrap: loaded {} rule(s) from control plane", defs.len());
            Ok(Some(defs))
        }
        Ok(Err(err)) => handle_fetch_failure(config, err).await,
        Err(_) => {
            handle_fetch_failure(config, anyhow::anyhow!("routes bootstrap request timed out")).await
        }
    }
}

async fn handle_fetch_failure(config: &RoutesBootstrap, err: impl std::fmt::Display) -> Result<Option<Vec<Proxy>>> {
    tracing::warn!("routes bootstrap: fetch failed: {err}");

    if let Some(ref cache_path) = config.cache_path {
        match read_cache(cache_path) {
            Ok(doc) => {
                let defs = doc.into_proxy_defs()?;
                tracing::info!(
                    "routes bootstrap: using {} rule(s) from cache {}",
                    defs.len(),
                    cache_path.display()
                );
                return Ok(Some(defs));
            }
            Err(cache_err) => {
                tracing::warn!("routes bootstrap: cache read failed: {cache_err:#}");
            }
        }
    }

    match config.on_failure.unwrap_or_default() {
        RoutesBootstrapOnFailure::Fail => {
            bail!("routes bootstrap: fetch failed and no cache available: {err}");
        }
        RoutesBootstrapOnFailure::CacheThenFile | RoutesBootstrapOnFailure::FileOnly => {
            tracing::info!("routes bootstrap: falling back to TOML [[advanced.proxies]] only");
            Ok(None)
        }
    }
}

async fn http_fetch_document(
    config: &RoutesBootstrap,
    weavegate_version: &str,
    tls: &ProxyTlsSettings,
) -> Result<RoutesDocument> {
    let uri = build_bootstrap_uri(&config.url, config.client_id.as_deref(), weavegate_version)?;

    let client = build_bootstrap_client(tls)?;

    let mut req = hyper::Request::builder()
        .method(hyper::Method::GET)
        .uri(uri)
        .header(ACCEPT, "application/json")
        .header(USER_AGENT, format!("weavegate/{weavegate_version}"));

    if let Some(ref env_name) = config.token_env {
        let token = std::env::var(env_name).with_context(|| {
            format!("routes bootstrap: environment variable '{env_name}' is not set")
        })?;
        if !token.is_empty() {
            req = req.header(AUTHORIZATION, format!("Bearer {token}"));
        }
    }

    let req = req.body(Body::empty())?;

    let resp = client
        .request(req)
        .await
        .context("routes bootstrap HTTP request failed")?;

    if !resp.status().is_success() {
        bail!(
            "routes bootstrap: control plane returned HTTP {}",
            resp.status()
        );
    }

    let body = hyper::body::to_bytes(resp.into_body())
        .await
        .context("routes bootstrap: failed to read response body")?;

    let doc: RoutesDocument =
        serde_json::from_slice(&body).context("routes bootstrap: invalid JSON response")?;

    Ok(doc)
}

fn build_bootstrap_uri(
    base: &str,
    client_id: Option<&str>,
    weavegate_version: &str,
) -> Result<Uri> {
    let mut url = base.to_string();
    let sep = if base.contains('?') { '&' } else { '?' };
    url.push(sep);
    url.push_str(&format!("weavegate={}", url_encode_component(weavegate_version)));
    if let Some(client) = client_id.filter(|s| !s.is_empty()) {
        url.push('&');
        url.push_str(&format!("client={}", url_encode_component(client)));
    }
    url.parse::<Uri>()
        .with_context(|| format!("routes bootstrap: invalid URL: {url}"))
}

fn url_encode_component(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

fn write_cache(path: &Path, doc: &RoutesDocument) -> Result<()> {
    let json = serde_json::to_vec_pretty(doc)?;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(dir) = parent {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn read_cache(path: &Path) -> Result<RoutesDocument> {
    let data = std::fs::read(path)
        .with_context(|| format!("routes bootstrap: cannot read cache {}", path.display()))?;
    serde_json::from_slice(&data).context("routes bootstrap: invalid cache JSON")
}
