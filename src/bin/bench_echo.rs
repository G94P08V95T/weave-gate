// SPDX-License-Identifier: MIT OR Apache-2.0
//! Minimal async HTTP echo server for proxy benchmarks (returns request path as body).

use hyper::{
    Body, Request, Response, Server, StatusCode,
    service::{make_service_fn, service_fn},
};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::process;

async fn echo(req: Request<Body>) -> Result<Response<Body>, Infallible> {
    let path = req.uri().path().as_bytes().to_vec();
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Length", path.len())
        .body(Body::from(path))
        .unwrap())
}

#[tokio::main]
async fn main() {
    let port: u16 = std::env::var("BENCH_ECHO_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(13_000);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap_or_else(|err| {
        eprintln!("bench-echo: failed to bind {addr}: {err}");
        process::exit(1);
    });
    let addr = listener.local_addr().unwrap();
    eprintln!("bench-echo: listening on http://{addr}");

    let make_svc = make_service_fn(|_| async { Ok::<_, Infallible>(service_fn(echo)) });
    let server = Server::from_tcp(listener.into_std().unwrap())
        .unwrap()
        .tcp_nodelay(true)
        .serve(make_svc);

    if let Err(err) = server.await {
        eprintln!("bench-echo: server error: {err}");
        process::exit(1);
    }
}
