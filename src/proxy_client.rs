// SPDX-License-Identifier: MIT OR Apache-2.0
// This file is part of WeaveGate.
// WeaveGate — frontend gateway and static file server.
// Copyright (C) 2019-present Jose Quintana <joseluisq.net>

//! Shared Hyper HTTP(S) client for reverse proxy and routes bootstrap.

use hyper::{
    Body, Client,
    client::HttpConnector,
};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use rustls::{ClientConfig, RootCertStore};
use rustls_pemfile::certs;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::time::Duration;

use crate::settings::{ProxySettings, ProxyTlsSettings};
use crate::{Context, Result};

/// Hyper client that supports both `http://` and `https://` upstreams.
pub type ProxyHttpClient = Client<HttpsConnector<HttpConnector>, Body>;

/// Builds a pooled client for reverse proxy upstream requests (HTTP and HTTPS).
pub fn build_proxy_client(settings: &ProxySettings) -> Result<ProxyHttpClient> {
    let mut http = HttpConnector::new();
    http.set_nodelay(true);
    http.set_keepalive(Some(Duration::from_secs(settings.pool_idle_timeout_secs)));
    http.enforce_http(false);

    let https = build_https_connector(&settings.tls, http)?;

    Ok(Client::builder()
        .http1_title_case_headers(true)
        .pool_max_idle_per_host(settings.pool_max_idle_per_host)
        .pool_idle_timeout(Duration::from_secs(settings.pool_idle_timeout_secs))
        .build(https))
}

/// Lightweight client for control-plane bootstrap GET (HTTP/HTTPS).
pub(crate) fn build_bootstrap_client(tls: &ProxyTlsSettings) -> Result<ProxyHttpClient> {
    let mut http = HttpConnector::new();
    http.set_nodelay(true);
    http.enforce_http(false);

    let https = build_https_connector(tls, http)?;
    Ok(Client::builder().build(https))
}

fn build_https_connector(
    tls: &ProxyTlsSettings,
    http: HttpConnector,
) -> Result<HttpsConnector<HttpConnector>> {
    if tls.ca_files.is_empty() {
        return Ok(HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .wrap_connector(http));
    }

    let roots = load_root_cert_store(tls)?;
    let config = ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(roots)
        .with_no_client_auth();

    Ok(HttpsConnectorBuilder::new()
        .with_tls_config(config)
        .https_or_http()
        .enable_http1()
        .wrap_connector(http))
}

fn load_root_cert_store(tls: &ProxyTlsSettings) -> Result<RootCertStore> {
    let mut roots = RootCertStore::empty();

    if tls.use_webpki_roots {
        roots.add_trust_anchors(webpki_roots::TLS_SERVER_ROOTS.iter().map(|ta| {
            rustls::OwnedTrustAnchor::from_subject_spki_name_constraints(
                ta.subject,
                ta.spki,
                ta.name_constraints,
            )
        }));
    }

    for path in &tls.ca_files {
        load_pem_ca_file(path, &mut roots)?;
    }

    if roots.is_empty() {
        bail!("proxy-tls: no trust anchors loaded");
    }

    Ok(roots)
}

fn load_pem_ca_file(path: &Path, roots: &mut RootCertStore) -> Result<()> {
    let file = File::open(path)
        .with_context(|| format!("proxy-tls: cannot open CA file {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let der_certs = certs(&mut reader).with_context(|| {
        format!("proxy-tls: cannot read PEM from CA file {}", path.display())
    })?;

    if der_certs.is_empty() {
        bail!(
            "proxy-tls: no certificates found in CA file {}",
            path.display()
        );
    }

    let added = der_certs.len();
    for der in der_certs {
        roots.add(&rustls::Certificate(der))?;
    }

    tracing::debug!(
        "proxy-tls: loaded {added} certificate(s) from {}",
        path.display()
    );
    Ok(())
}
