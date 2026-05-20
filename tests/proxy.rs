#![forbid(unsafe_code)]
#![deny(warnings)]
#![deny(rust_2018_idioms)]
#![deny(dead_code)]

use clap::Parser;
use hyper::{
    Body, Request, Response, Server, StatusCode,
    service::{make_service_fn, service_fn},
};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use weavegate::proxy_client::build_proxy_client;
use weavegate::settings::cli::General;
use weavegate::settings::{Advanced, ProxyBalancer, ProxyRule, ProxySettings, upstream_uri_template};
use weavegate::testing::fixtures::{fixture_req_handler, fixture_req_handler_opts};

async fn echo_upstream(req: Request<Body>) -> Result<Response<Body>, Infallible> {
    let path = req.uri().path();
    Ok(Response::builder()
        .status(200)
        .body(Body::from(path.to_string()))
        .unwrap())
}

async fn echo_upstream_authority(req: Request<Body>) -> Result<Response<Body>, Infallible> {
    // Proxied HTTP/1.1 requests often use a path-only URI; Host identifies the instance.
    let tag = req
        .headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .map(str::to_string)
        .unwrap_or_default();
    Ok(Response::builder()
        .status(200)
        .body(Body::from(tag))
        .unwrap())
}

async fn spawn_tagged_echo_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let make_svc = make_service_fn(|_| async {
        Ok::<_, Infallible>(service_fn(echo_upstream_authority))
    });
    let server = Server::from_tcp(listener.into_std().unwrap())
        .unwrap()
        .serve(make_svc);
    let handle = tokio::spawn(async move {
        server.await.unwrap();
    });
    (addr, handle)
}

async fn spawn_echo_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let make_svc = make_service_fn(|_| async {
        Ok::<_, Infallible>(service_fn(echo_upstream))
    });
    let server = Server::from_tcp(listener.into_std().unwrap())
        .unwrap()
        .serve(make_svc);
    let handle = tokio::spawn(async move {
        server.await.unwrap();
    });
    (addr, handle)
}

fn proxy_settings(upstream: SocketAddr) -> (General, Advanced) {
    let target = format!("http://{upstream}");
    let general = General::parse_from([
        "weavegate",
        "--host",
        "127.0.0.1",
        "--port",
        "8787",
        "--root",
        "tests/fixtures/public",
        "--log-level",
        "error",
        "--compression",
        "false",
    ]);
    let target_uri: hyper::Uri = target.parse().unwrap();
    let uri_templates = vec![upstream_uri_template(&target_uri, Some("/api")).unwrap()];
    let advanced = Advanced {
        proxies: Some(vec![ProxyRule {
            host: None,
            source: globset::Glob::new("/api/**").unwrap().compile_matcher(),
            balancer: ProxyBalancer::new(None, vec![target_uri]).unwrap(),
            strip_prefix: Some("/api".to_string()),
            uri_templates,
        }]),
        ..Default::default()
    };
    (general, advanced)
}

#[tokio::test]
async fn proxy_forwards_to_upstream() {
    let (upstream_addr, _server) = spawn_echo_server().await;
    let (general, mut advanced) = proxy_settings(upstream_addr);
    let upstream_uri: hyper::Uri = format!("http://{upstream_addr}").parse().unwrap();
    let templates = vec![upstream_uri_template(&upstream_uri, Some("/api")).unwrap()];
    let rule = &mut advanced.proxies.as_mut().unwrap()[0];
    rule.balancer = ProxyBalancer::new(None, vec![upstream_uri]).unwrap();
    rule.uri_templates = templates;

    let mut opts = fixture_req_handler_opts(general, Some(advanced));
    opts.proxy_client = Some(Arc::new(
        build_proxy_client(&ProxySettings::default()).unwrap(),
    ));
    let req_handler = fixture_req_handler(opts);

    let mut req = Request::builder()
        .method("GET")
        .uri("http://localhost/api/users/42")
        .header("Host", "localhost")
        .body(Body::empty())
        .unwrap();

    let resp = req_handler
        .handle(&mut req, None)
        .await
        .expect("proxy request should succeed");

    assert_eq!(resp.status(), StatusCode::OK);
    let body = hyper::body::to_bytes(resp.into_body()).await.unwrap();
    assert_eq!(body.as_ref(), b"/users/42");
}

#[tokio::test]
async fn proxy_round_robin_between_instances() {
    let (addr1, _s1) = spawn_tagged_echo_server().await;
    let (addr2, _s2) = spawn_tagged_echo_server().await;
    let general = General::parse_from([
        "weavegate",
        "--host",
        "127.0.0.1",
        "--port",
        "8787",
        "--root",
        "tests/fixtures/public",
        "--log-level",
        "error",
        "--compression",
        "false",
    ]);
    let uri1: hyper::Uri = format!("http://{addr1}").parse().unwrap();
    let uri2: hyper::Uri = format!("http://{addr2}").parse().unwrap();
    let uri_templates = vec![
        upstream_uri_template(&uri1, Some("/api")).unwrap(),
        upstream_uri_template(&uri2, Some("/api")).unwrap(),
    ];
    let advanced = Advanced {
        proxies: Some(vec![ProxyRule {
            host: None,
            source: globset::Glob::new("/api/**").unwrap().compile_matcher(),
            balancer: ProxyBalancer::new(
                Some("api-service".to_string()),
                vec![uri1, uri2],
            )
            .unwrap(),
            strip_prefix: Some("/api".to_string()),
            uri_templates,
        }]),
        ..Default::default()
    };

    let mut opts = fixture_req_handler_opts(general, Some(advanced));
    opts.proxy_client = Some(Arc::new(
        build_proxy_client(&ProxySettings::default()).unwrap(),
    ));
    let req_handler = fixture_req_handler(opts);

    let tag1 = addr1.to_string();
    let tag2 = addr2.to_string();
    let mut seen = Vec::new();

    for _ in 0..4 {
        let mut req = Request::builder()
            .method("GET")
            .uri("http://localhost/api/ping")
            .header("Host", "localhost")
            .body(Body::empty())
            .unwrap();

        let resp = req_handler.handle(&mut req, None).await.unwrap();
        let body = hyper::body::to_bytes(resp.into_body()).await.unwrap();
        seen.push(String::from_utf8_lossy(&body).to_string());
    }

    assert_eq!(seen[0], tag1);
    assert_eq!(seen[1], tag2);
    assert_eq!(seen[2], tag1);
    assert_eq!(seen[3], tag2);
}

#[tokio::test]
async fn proxy_upgrade_returns_switching_protocols() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = listener.local_addr().unwrap();
    let make_svc = make_service_fn(|_| async {
        Ok::<_, Infallible>(service_fn(|_req: Request<Body>| async {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(StatusCode::SWITCHING_PROTOCOLS)
                    .header("connection", "upgrade")
                    .header("upgrade", "websocket")
                    .header("sec-websocket-accept", "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=")
                    .body(Body::empty())
                    .unwrap(),
            )
        }))
    });
    let server = Server::from_tcp(listener.into_std().unwrap())
        .unwrap()
        .serve(make_svc);
    let _server = tokio::spawn(async move {
        server.await.unwrap();
    });

    let (general, mut advanced) = proxy_settings(upstream_addr);
    let upstream_uri: hyper::Uri = format!("http://{upstream_addr}").parse().unwrap();
    let rule = &mut advanced.proxies.as_mut().unwrap()[0];
    rule.uri_templates = vec![upstream_uri_template(&upstream_uri, Some("/ws")).unwrap()];
    rule.balancer = ProxyBalancer::new(None, vec![upstream_uri]).unwrap();
    rule.source = globset::Glob::new("/ws/**").unwrap().compile_matcher();
    rule.strip_prefix = Some("/ws".to_string());

    let mut opts = fixture_req_handler_opts(general, Some(advanced));
    opts.proxy_client = Some(Arc::new(
        build_proxy_client(&ProxySettings::default()).unwrap(),
    ));
    let req_handler = fixture_req_handler(opts);

    let mut req = Request::builder()
        .method("GET")
        .uri("http://localhost/ws/socket")
        .header("Host", "localhost")
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("sec-websocket-version", "13")
        .body(Body::empty())
        .unwrap();

    let resp = req_handler
        .handle(&mut req, None)
        .await
        .expect("upgrade proxy should succeed");

    assert_eq!(resp.status(), StatusCode::SWITCHING_PROTOCOLS);
    assert_eq!(
        resp.headers().get("upgrade").and_then(|v| v.to_str().ok()),
        Some("websocket")
    );
}

#[tokio::test]
async fn proxy_non_matching_serves_static() {
    let (upstream_addr, _server) = spawn_echo_server().await;
    let (general, advanced) = proxy_settings(upstream_addr);
    let mut opts = fixture_req_handler_opts(general, Some(advanced));
    opts.proxy_client = Some(Arc::new(
        build_proxy_client(&ProxySettings::default()).unwrap(),
    ));
    let req_handler = fixture_req_handler(opts);

    let mut req = Request::builder()
        .method("GET")
        .uri("http://localhost/index.htm")
        .header("Host", "localhost")
        .body(Body::empty())
        .unwrap();

    let resp = req_handler
        .handle(&mut req, None)
        .await
        .expect("static file request should succeed");

    assert_eq!(resp.status(), StatusCode::OK);
}
