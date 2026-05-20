// SPDX-License-Identifier: MIT OR Apache-2.0
// This file is part of WeaveGate.
// WeaveGate — frontend gateway and static file server.
// Copyright (C) 2019-present Jose Quintana <joseluisq.net>

//! Reverse proxy module: forwards HTTP requests to configured upstream backends.
//! Supports WebSocket and other HTTP protocol upgrades, plus streaming (e.g. SSE).
//!

use hyper::{
    Body, Request, Response, StatusCode, Uri,
    header::{self, HeaderMap, HeaderName, HeaderValue, HOST},
    upgrade::OnUpgrade,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Semaphore;

use crate::{
    Context, Error, Result,
    handler::RequestHandlerOpts,
    proxy_client::{ProxyHttpClient, build_proxy_client},
    proxy_compile::compile_proxy_rules,
    proxy_sort::sort_proxy_defs_by_specificity,
    routes::fetch_routes_defs,
    settings::{Advanced, ProxyRule, UpstreamUriTemplate},
};

/// Shared HTTP(S) client for upstream requests.
pub(crate) type HttpClient = ProxyHttpClient;

/// Merges TOML and control-plane proxy definitions, sorts by specificity, and compiles rules.
pub async fn resolve_rules(advanced: &mut Option<Advanced>) -> Result<()> {
    let Some(adv) = advanced else {
        return Ok(());
    };

    if adv.proxies.is_some() {
        return Ok(());
    }

    let mut defs = adv.proxy_defs.clone().unwrap_or_default();

    if let Some(ref bootstrap) = adv.routes_bootstrap {
        if let Some(remote) =
            fetch_routes_defs(bootstrap, env!("CARGO_PKG_VERSION"), &adv.proxy_settings.tls)
                .await?
        {
            defs.extend(remote);
        }
    }

    if defs.is_empty() {
        return Ok(());
    }

    sort_proxy_defs_by_specificity(&mut defs);
    adv.proxies = Some(compile_proxy_rules(defs)?);
    Ok(())
}

/// Initializes the reverse proxy client when proxy rules are configured.
pub(crate) fn init(handler_opts: &mut RequestHandlerOpts) -> Result<()> {
    let advanced = handler_opts.advanced_opts.as_mut();
    let proxies = advanced
        .as_ref()
        .and_then(|a| a.proxies.as_deref());

    if let Some(rules) = proxies {
        if !rules.is_empty() {
            let settings = advanced
                .as_ref()
                .map(|a| a.proxy_settings.clone())
                .unwrap_or_default();

            let client = build_proxy_client(&settings)?;

            handler_opts.proxy_client = Some(Arc::new(client));
            handler_opts.proxy_settings = Some(settings.clone());
            handler_opts.proxy_first = settings.proxy_first;
            handler_opts.upgrade_tunnel_semaphore = settings
                .max_upgrade_tunnels
                .map(|n| Arc::new(Semaphore::new(n)));

            tracing::info!(
                "reverse proxy: enabled ({} rule(s), pool_idle_per_host={}, pool_idle_timeout={}s, proxy_first={}, max_upgrade_tunnels={})",
                rules.len(),
                settings.pool_max_idle_per_host,
                settings.pool_idle_timeout_secs,
                settings.proxy_first,
                settings
                    .max_upgrade_tunnels
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "unlimited".to_string()),
            );
        }
    } else {
        tracing::debug!("reverse proxy: disabled (no rules)");
    }

    Ok(())
}

/// Forwards the request to an upstream backend when a proxy rule matches.
pub(crate) async fn pre_process(
    opts: &RequestHandlerOpts,
    req: &mut Request<Body>,
    remote_addr: Option<SocketAddr>,
) -> Option<Result<Response<Body>, Error>> {
    let rule = find_rule(opts, req)?;
    let client = opts.proxy_client.as_ref()?;

    tracing::debug!(
        "proxy match: service={} targets=[{}] instances={} method={} uri={} upgrade={}",
        rule.balancer
            .name
            .as_deref()
            .unwrap_or("proxy"),
        rule.balancer.targets_display(),
        rule.balancer.len(),
        req.method(),
        req.uri(),
        is_protocol_upgrade(req)
    );

    Some(
        if is_protocol_upgrade(req) {
            forward_upgrade(opts, client, rule, req, remote_addr).await
        } else {
            forward_http(client, rule, req, remote_addr).await
        }
        .map_err(|err| {
            tracing::error!("proxy error: {err:#}");
            err
        }),
    )
}

fn find_rule<'a>(opts: &'a RequestHandlerOpts, req: &Request<Body>) -> Option<&'a ProxyRule> {
    let proxies = opts.advanced_opts.as_ref()?.proxies.as_deref()?;
    let uri_path = req.uri().path();
    let request_host = request_host(req)?;

    for rule in proxies {
        if let Some(ref host) = rule.host {
            if host != request_host {
                continue;
            }
        }
        if rule.source.is_match(uri_path) {
            return Some(rule);
        }
    }
    None
}

fn request_host<T>(req: &Request<T>) -> Option<&str> {
    if let Some(authority) = req.uri().authority() {
        return Some(authority.host());
    }

    let host_header = req.headers().get(HOST)?.to_str().ok()?;
    Some(
        host_header
            .rsplit_once(':')
            .and_then(|(potential_host, potential_port)| {
                potential_port
                    .parse::<u16>()
                    .ok()
                    .map(|_| potential_host)
            })
            .unwrap_or(host_header),
    )
}

/// Returns true when the request is an HTTP protocol upgrade (e.g. WebSocket).
pub(crate) fn is_protocol_upgrade<T>(req: &Request<T>) -> bool {
    if !req.headers().contains_key(header::UPGRADE) {
        return false;
    }

    connection_header_has_token(req.headers(), "upgrade")
}

/// Returns true for long-lived streaming HTTP such as Server-Sent Events.
fn is_streaming_request<T>(req: &Request<T>) -> bool {
    req.headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|accept| {
            accept
                .to_ascii_lowercase()
                .contains("text/event-stream")
        })
}

fn connection_header_has_token(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get(header::CONNECTION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|part| part.trim().eq_ignore_ascii_case(token))
        })
}

fn upstream_uri_for_rule(rule: &ProxyRule, req_uri: &Uri) -> Result<Uri> {
    let index = rule.balancer.next_index();
    let template = rule
        .uri_templates
        .get(index)
        .ok_or_else(|| Error::msg("proxy URI template index out of range"))?;
    template.build(req_uri)
}

async fn forward_http(
    client: &HttpClient,
    rule: &ProxyRule,
    req: &mut Request<Body>,
    remote_addr: Option<SocketAddr>,
) -> Result<Response<Body>, Error> {
    let upstream_uri = upstream_uri_for_rule(rule, req.uri())?;
    let body = std::mem::replace(req.body_mut(), Body::empty());
    let streaming = is_streaming_request(req);

    let mut upstream_req = Request::builder()
        .method(req.method().clone())
        .uri(upstream_uri.clone())
        .body(body)?;

    copy_forwardable_headers(req.headers(), upstream_req.headers_mut());
    set_upstream_host(upstream_req.headers_mut(), &upstream_uri)?;
    append_forwarded_headers(upstream_req.headers_mut(), req, remote_addr);
    remove_hop_by_hop_request_headers(upstream_req.headers_mut());

    let upstream_resp = client
        .request(upstream_req)
        .await
        .with_context(|| format!("proxy request failed for upstream {upstream_uri}"))?;

    let (parts, body) = upstream_resp.into_parts();
    let mut resp = Response::from_parts(parts, body);

    if streaming {
        preserve_streaming_response_headers(resp.headers_mut());
    } else {
        remove_hop_by_hop_response_headers(resp.headers_mut());
    }

    Ok(resp)
}

async fn forward_upgrade(
    opts: &RequestHandlerOpts,
    client: &HttpClient,
    rule: &ProxyRule,
    req: &mut Request<Body>,
    remote_addr: Option<SocketAddr>,
) -> Result<Response<Body>, Error> {
    if let Some(sem) = &opts.upgrade_tunnel_semaphore {
        let Ok(permit) = sem.clone().try_acquire_owned() else {
            return Ok(Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .body(Body::empty())?);
        };
        return forward_upgrade_inner(opts, client, rule, req, remote_addr, Some(permit)).await;
    }

    forward_upgrade_inner(opts, client, rule, req, remote_addr, None).await
}

async fn forward_upgrade_inner(
    _opts: &RequestHandlerOpts,
    client: &HttpClient,
    rule: &ProxyRule,
    req: &mut Request<Body>,
    remote_addr: Option<SocketAddr>,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
) -> Result<Response<Body>, Error> {
    let upstream_uri = upstream_uri_for_rule(rule, req.uri())?;
    let method = req.method().clone();
    let body = std::mem::replace(req.body_mut(), Body::empty());
    let request_headers = req.headers().clone();

    let mut upstream_req = Request::builder()
        .method(method)
        .uri(upstream_uri.clone())
        .body(body)?;

    copy_upgrade_request_headers(&request_headers, upstream_req.headers_mut());
    set_upstream_host(upstream_req.headers_mut(), &upstream_uri)?;
    append_forwarded_headers(upstream_req.headers_mut(), req, remote_addr);

    let on_client = hyper::upgrade::on(req);

    let mut upstream_resp = client
        .request(upstream_req)
        .await
        .with_context(|| format!("proxy upgrade request failed for upstream {upstream_uri}"))?;

    if upstream_resp.status() != StatusCode::SWITCHING_PROTOCOLS {
        tracing::debug!(
            "upstream did not switch protocols (status={}), returning as HTTP",
            upstream_resp.status()
        );
        let (parts, body) = upstream_resp.into_parts();
        return Ok(Response::from_parts(parts, body));
    }

    let on_server = hyper::upgrade::on(&mut upstream_resp);
    let (parts, _body) = upstream_resp.into_parts();
    let mut resp = Response::from_parts(parts, Body::empty());
    preserve_upgrade_response_headers(resp.headers_mut());

    let upgrade_kind = resp
        .headers()
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_owned();

    tokio::spawn(async move {
        let _permit = permit;
        if let Err(err) = tunnel_upgrades(on_client, on_server, &upgrade_kind).await {
            tracing::warn!("proxy upgrade tunnel ({upgrade_kind}) ended: {err:#}");
        }
    });

    Ok(resp)
}

async fn tunnel_upgrades(
    client: OnUpgrade,
    server: OnUpgrade,
    upgrade_kind: &str,
) -> Result<()> {
    let (mut client_io, mut server_io) =
        tokio::try_join!(client, server).with_context(|| {
            format!("failed to complete protocol upgrade handshake ({upgrade_kind})")
        })?;

    match tokio::io::copy_bidirectional(&mut client_io, &mut server_io).await {
        Ok((client_bytes, server_bytes)) => {
            tracing::debug!(
                "proxy upgrade tunnel closed ({upgrade_kind}): client={client_bytes}B server={server_bytes}B"
            );
        }
        Err(err) => {
            tracing::debug!("proxy upgrade tunnel I/O ({upgrade_kind}): {err}");
        }
    }

    Ok(())
}

/// Builds the upstream request URI from a precomputed template and incoming URI.
pub(crate) fn build_upstream_uri_from_template(
    template: &UpstreamUriTemplate,
    req_uri: &Uri,
) -> Result<Uri> {
    let path_and_query = req_uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");

    let mut path_and_query = if let Some(ref prefix) = template.strip_prefix {
        path_and_query
            .strip_prefix(prefix)
            .unwrap_or(path_and_query)
    } else {
        path_and_query
    };

    if path_and_query.is_empty() {
        path_and_query = "/";
    } else if !path_and_query.starts_with('/') {
        return Err(Error::msg(format!(
            "invalid path after strip-prefix: {path_and_query}"
        )));
    }

    let merged_path = if template.base_path.is_empty() || template.base_path == "/" {
        path_and_query.to_string()
    } else {
        let base = template.base_path.trim_end_matches('/');
        format!("{base}{path_and_query}")
    };

    let full = format!("{}{}", template.origin, merged_path);
    full.parse::<Uri>()
        .with_context(|| format!("invalid upstream URI: {full}"))
}

/// Builds the upstream request URI from a selected upstream base and incoming URI.
#[cfg(test)]
pub(crate) fn build_upstream_uri(
    upstream_base: &Uri,
    strip_prefix: Option<&str>,
    req_uri: &Uri,
) -> Result<Uri> {
    let authority = upstream_base
        .authority()
        .expect("validated at startup");
    let scheme = upstream_base.scheme_str().expect("validated at startup");
    let template = UpstreamUriTemplate {
        origin: format!("{scheme}://{authority}"),
        base_path: {
            let p = upstream_base.path();
            if p.is_empty() {
                "/".to_string()
            } else {
                p.to_string()
            }
        },
        strip_prefix: strip_prefix.map(str::to_string),
    };
    build_upstream_uri_from_template(&template, req_uri)
}

fn copy_forwardable_headers(src: &HeaderMap, dst: &mut HeaderMap) {
    for (name, value) in src.iter() {
        if is_hop_by_hop_header(name) || name == HOST {
            continue;
        }
        dst.insert(name, value.clone());
    }
}

fn copy_upgrade_request_headers(src: &HeaderMap, dst: &mut HeaderMap) {
    for (name, value) in src.iter() {
        if name == HOST {
            continue;
        }
        dst.insert(name, value.clone());
    }
}

fn set_upstream_host(headers: &mut HeaderMap, upstream_uri: &Uri) -> Result<()> {
    let authority = upstream_uri
        .authority()
        .ok_or_else(|| Error::msg("upstream URI has no authority"))?;
    let host = HeaderValue::from_str(authority.as_str())
        .with_context(|| format!("invalid upstream Host header value: {authority}"))?;
    headers.insert(HOST, host);
    Ok(())
}

fn append_forwarded_headers(
    headers: &mut HeaderMap,
    req: &Request<Body>,
    remote_addr: Option<SocketAddr>,
) {
    if let Some(addr) = remote_addr {
        if let Ok(value) = HeaderValue::from_str(&addr.ip().to_string()) {
            headers.insert("X-Forwarded-For", value);
        }
    } else if let Some(existing) = req.headers().get("X-Forwarded-For") {
        headers.insert("X-Forwarded-For", existing.clone());
    }

    let proto = req
        .headers()
        .get("X-Forwarded-Proto")
        .cloned()
        .or_else(|| {
            req.uri()
                .scheme_str()
                .map(|s| HeaderValue::from_str(s).ok())
                .flatten()
        })
        .unwrap_or_else(|| HeaderValue::from_static("http"));
    headers.insert("X-Forwarded-Proto", proto);

    if let Some(host) = req.headers().get(HOST) {
        headers.insert("X-Forwarded-Host", host.clone());
    }
}

fn remove_hop_by_hop_request_headers(headers: &mut HeaderMap) {
    headers.remove(header::CONNECTION);
    headers.remove(header::PROXY_AUTHORIZATION);
    headers.remove(header::TE);
    headers.remove(header::TRAILER);
    headers.remove(header::TRANSFER_ENCODING);
    headers.remove(header::UPGRADE);
    headers.remove(HeaderName::from_static("keep-alive"));
    headers.remove(HeaderName::from_static("proxy-authenticate"));
    headers.remove(HeaderName::from_static("proxy-connection"));
}

fn remove_hop_by_hop_response_headers(headers: &mut HeaderMap) {
    remove_hop_by_hop_request_headers(headers);
}

fn preserve_upgrade_response_headers(_headers: &mut HeaderMap) {}

fn preserve_streaming_response_headers(headers: &mut HeaderMap) {
    headers.remove(header::PROXY_AUTHORIZATION);
    headers.remove(HeaderName::from_static("proxy-authenticate"));
    headers.remove(HeaderName::from_static("proxy-connection"));
}

fn is_hop_by_hop_header(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailers"
            | "transfer-encoding"
            | "upgrade"
            | "proxy-connection"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upstream(target: &str) -> Uri {
        target.parse().unwrap()
    }

    #[test]
    fn build_upstream_uri_with_strip_prefix() {
        let base = upstream("http://127.0.0.1:3000");
        let uri: Uri = "http://localhost/api/users/1?q=1".parse().unwrap();
        let got = build_upstream_uri(&base, Some("/api"), &uri).unwrap();
        assert_eq!(got.to_string(), "http://127.0.0.1:3000/users/1?q=1");
    }

    #[test]
    fn build_upstream_uri_from_template_matches_legacy() {
        let base = upstream("http://backend:8080/v1");
        let uri: Uri = "http://localhost/users".parse().unwrap();
        let template = UpstreamUriTemplate {
            origin: "http://backend:8080".to_string(),
            base_path: "/v1".to_string(),
            strip_prefix: None,
        };
        assert_eq!(
            build_upstream_uri_from_template(&template, &uri).unwrap().to_string(),
            build_upstream_uri(&base, None, &uri).unwrap().to_string(),
        );
    }

    #[test]
    fn proxy_balancer_round_robin() {
        use crate::settings::ProxyBalancer;

        let b = ProxyBalancer::new(
            Some("api".to_string()),
            vec![
                upstream("http://127.0.0.1:3001"),
                upstream("http://127.0.0.1:3002"),
                upstream("http://127.0.0.1:3003"),
            ],
        )
        .unwrap();

        assert_eq!(b.len(), 3);
        assert_eq!(b.next_index(), 0);
        assert_eq!(b.next_index(), 1);
        assert_eq!(b.next_index(), 2);
        assert_eq!(b.next_index(), 0);
    }

    #[test]
    fn detects_websocket_upgrade_request() {
        let req = Request::builder()
            .uri("http://localhost/ws")
            .header(header::CONNECTION, "Upgrade")
            .header(header::UPGRADE, "websocket")
            .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
            .header("Sec-WebSocket-Version", "13")
            .body(Body::empty())
            .unwrap();
        assert!(is_protocol_upgrade(&req));
    }
}
