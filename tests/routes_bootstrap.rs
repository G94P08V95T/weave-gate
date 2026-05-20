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
use weavegate::routes::RoutesDocument;
use weavegate::settings::cli::General;
use weavegate::settings::file::{RoutesBootstrap, RoutesBootstrapOnFailure};
use weavegate::settings::{Advanced, ProxySettings};
use weavegate::testing::fixtures::{fixture_req_handler, fixture_req_handler_opts};

async fn echo_upstream(req: Request<Body>) -> Result<Response<Body>, Infallible> {
    let path = req.uri().path();
    Ok(Response::builder()
        .status(200)
        .body(Body::from(path.to_string()))
        .unwrap())
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

async fn spawn_routes_api(body: Arc<String>) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let body = body.clone();
    let make_svc = make_service_fn(move |_| {
        let body = body.clone();
        async move {
            Ok::<_, Infallible>(service_fn(move |_req: Request<Body>| {
                let body = body.clone();
                async move {
                    Ok::<_, Infallible>(
                        Response::builder()
                            .status(200)
                            .header("content-type", "application/json")
                            .body(Body::from(body.to_string()))
                            .unwrap(),
                    )
                }
            }))
        }
    });
    let server = Server::from_tcp(listener.into_std().unwrap())
        .unwrap()
        .serve(make_svc);
    let handle = tokio::spawn(async move {
        server.await.unwrap();
    });
    (addr, handle)
}

fn routes_json(users_target: &str, gateway_target: &str) -> String {
    format!(
        r#"{{
  "version": 1,
  "apps": [
    {{
      "id": "app1",
      "routes": [
        {{
          "name": "api-gateway",
          "source": "/app1/api/**",
          "target": "{gateway_target}",
          "strip-prefix": "/app1/api"
        }},
        {{
          "name": "user-service",
          "source": "/app1/api/users/**",
          "target": "{users_target}",
          "strip-prefix": "/app1/api/users"
        }}
      ]
    }}
  ]
}}"#,
    )
}

async fn run_proxy_request(
    advanced: Advanced,
    path: &str,
) -> (StatusCode, String) {
    let general = General::parse_from([
        "weavegate",
        "--host",
        "127.0.0.1",
        "--port",
        "8787",
        "--root",
        "./docker/public",
    ]);
    let mut advanced = Some(advanced);
    weavegate::proxy::resolve_rules(&mut advanced).await.unwrap();
    let mut opts = fixture_req_handler_opts(general, advanced);
    opts.proxy_client = Some(Arc::new(
        build_proxy_client(&ProxySettings::default()).unwrap(),
    ));
    let req_handler = fixture_req_handler(opts);

    let mut req = Request::builder()
        .method("GET")
        .uri(format!("http://127.0.0.1:8787{path}"))
        .header("Host", "127.0.0.1:8787")
        .body(Body::empty())
        .unwrap();

    let resp = req_handler
        .handle(&mut req, None)
        .await
        .expect("proxy request should succeed");
    let status = resp.status();
    let body = hyper::body::to_bytes(resp.into_body()).await.unwrap();
    (status, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test]
async fn bootstrap_sorts_specific_routes_before_gateway() {
    let (users_addr, users_handle) = spawn_echo_server().await;
    let (gateway_addr, gateway_handle) = spawn_echo_server().await;
    let users_target = format!("http://{users_addr}");
    let gateway_target = format!("http://{gateway_addr}");

    let routes_body = Arc::new(routes_json(&users_target, &gateway_target));
    let (routes_addr, routes_handle) = spawn_routes_api(routes_body).await;

    let bootstrap = RoutesBootstrap {
        url: format!("http://{routes_addr}/routes"),
        token_env: None,
        timeout_secs: Some(5),
        cache_path: None,
        on_failure: Some(RoutesBootstrapOnFailure::Fail),
        client_id: Some("test".to_string()),
    };

    let advanced = Advanced {
        routes_bootstrap: Some(bootstrap),
        ..Default::default()
    };

    let (status, body) = run_proxy_request(advanced, "/app1/api/users/me").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "/me");

    users_handle.abort();
    gateway_handle.abort();
    routes_handle.abort();
}

#[tokio::test]
async fn bootstrap_gateway_fallback_for_non_user_paths() {
    let (users_addr, users_handle) = spawn_echo_server().await;
    let (gateway_addr, gateway_handle) = spawn_echo_server().await;
    let routes_body = Arc::new(routes_json(
        &format!("http://{users_addr}"),
        &format!("http://{gateway_addr}"),
    ));
    let (routes_addr, routes_handle) = spawn_routes_api(routes_body).await;

    let bootstrap = RoutesBootstrap {
        url: format!("http://{routes_addr}/routes"),
        token_env: None,
        timeout_secs: Some(5),
        cache_path: None,
        on_failure: Some(RoutesBootstrapOnFailure::Fail),
        client_id: None,
    };

    let advanced = Advanced {
        routes_bootstrap: Some(bootstrap),
        ..Default::default()
    };

    let (status, body) = run_proxy_request(advanced, "/app1/api/reports/1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "/reports/1");

    users_handle.abort();
    gateway_handle.abort();
    routes_handle.abort();
}

#[tokio::test]
async fn bootstrap_cache_then_file_on_fetch_failure() {
    let (users_addr, users_handle) = spawn_echo_server().await;
    let (gateway_addr, gateway_handle) = spawn_echo_server().await;
    let doc = routes_json(
        &format!("http://{users_addr}"),
        &format!("http://{gateway_addr}"),
    );

    let cache_dir = std::env::temp_dir().join(format!(
        "weavegate-routes-cache-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&cache_dir).unwrap();
    let cache_path = cache_dir.join("routes.cache.json");
    std::fs::write(&cache_path, &doc).unwrap();

    let bootstrap = RoutesBootstrap {
        url: "http://127.0.0.1:1/unreachable".to_string(),
        token_env: None,
        timeout_secs: Some(1),
        cache_path: Some(cache_path.clone()),
        on_failure: Some(RoutesBootstrapOnFailure::CacheThenFile),
        client_id: None,
    };

    let advanced = Advanced {
        routes_bootstrap: Some(bootstrap),
        ..Default::default()
    };

    let (status, body) = run_proxy_request(advanced, "/app1/api/users/x").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "/x");

    let _ = std::fs::remove_dir_all(&cache_dir);
    users_handle.abort();
    gateway_handle.abort();
}

#[test]
fn proxy_tls_requires_ca_when_webpki_disabled() {
    use weavegate::settings::ProxyTlsSettings;
    use weavegate::settings::file::ProxyTls;

    let tls = ProxyTls {
        ca_file: None,
        ca_files: None,
        disable_webpki_roots: Some(true),
    };
    assert!(ProxyTlsSettings::from_file(&tls).is_err());
}

#[tokio::test]
async fn routes_document_rejects_unsupported_version() {
    let doc: RoutesDocument = serde_json::from_str(
        r#"{"version":2,"apps":[]}"#,
    )
    .unwrap();
    assert!(doc.into_proxy_defs().is_err());
}
