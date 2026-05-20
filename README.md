<div>
  <div align="center">
    <h1 align="center">WeaveGate</h1>
  </div>

<h4 align="center">
    A cross-platform frontend gateway: static files, reverse proxy, round-robin load balancing, and WebSocket forwarding
  </h4>
</div>

## Overview

**WeaveGate** is a lightweight production-ready frontend gateway. It serves static web assets, forwards API traffic to upstream backends (avoiding browser CORS), weaves requests across multiple instances with round-robin, and tunnels WebSocket / SSE long connections.

Built on [Hyper](https://github.com/hyperium/hyper) and [Tokio](https://github.com/tokio-rs/tokio). Configurable via CLI, environment variables, or TOML (`weavegate.toml`).

**Repository:** [github.com/G94P08V95T/weave-gate](https://github.com/G94P08V95T/weave-gate)

> **Note:** This project was forked and rebranded from [Static Web Server](https://github.com/static-web-server/static-web-server).

## Quick start

```sh
cargo build --release
./target/release/weavegate -w weavegate.example.toml
```

Or use the helper script:

```sh
./scripts/go.sh
```

## Gateway features (reverse proxy)

Configure services under `[advanced.proxies]` in `weavegate.toml`:

```toml
# Single upstream
[[advanced.proxies]]
name = "api-service"
source = "/api/**"
target = "http://127.0.0.1:3000"
strip-prefix = "/api"

# Multiple instances — round-robin
[[advanced.proxies]]
name = "auth-service"
source = "/auth/**"
targets = ["http://127.0.0.1:4001", "http://127.0.0.1:4002"]
strip-prefix = "/auth"
```

See [weavegate.example.toml](weavegate.example.toml) for a full example.

### Enterprise desktop: startup routes bootstrap

For a **group portal** on the user machine (`https://127.0.0.1:110/{appId}/...`), proxy rules can be loaded at startup from a control-plane HTTP API instead of only static TOML. WeaveGate defines the v1 JSON contract; your backend implements `GET` + JSON response.

- Per-app **A**: `/appId/api/users/**` → user microservice  
- Per-app **B**: `/appId/api/**` → corporate API gateway (catch-all)  
- Rules are merged with optional local `[[advanced.proxies]]` and sorted by path specificity  
- Upstream `target` URLs support **HTTP and HTTPS** (rustls / WebPKI roots); same for bootstrap `url`
- **mkcert**: set `[advanced.proxy-tls] ca-file` to `$(mkcert -CAROOT)/rootCA.pem` for local HTTPS (see [docs/routes-bootstrap-v1.md](docs/routes-bootstrap-v1.md))

See [docs/routes-bootstrap-v1.md](docs/routes-bootstrap-v1.md).

### Performance tuning (reverse proxy)

| Setting | Suggestion |
|---------|------------|
| `log-level` | `warn` in production (proxy match logs are at `debug`) |
| `threads-multiplier` | Start with `1` for API-heavy gateways; benchmark with `./scripts/bench-proxy.sh` |
| `proxy-pool-max-idle-per-host` | Raise when many concurrent clients hit the same upstream |
| `proxy-first` | `true` if most traffic is proxied API (skips redirects/rewrites before proxy) |

```sh
./scripts/bench-proxy.sh   # requires wrk, curl; uses Rust bench-echo upstream (v2)
```

The benchmark script starts `bench-echo` (Tokio/Hyper path echo) instead of Python, so results reflect gateway overhead rather than a weak mock upstream. It prints an RPS ratio (via-weavegate / direct-upstream); target is about **0.85+** on loopback with `Non-2xx = 0`.

## Static file features

- Built with [Rust](https://rust-lang.org)
- Optional GZip, Deflate, Brotli, Zstd compression
- HTTP/2 and TLS support
- Directory listing, CORS, Basic Auth, URL rewrites/redirects
- Prometheus metrics, health endpoint, graceful shutdown
- Virtual hosts, maintenance mode, pre-compressed static files

## Configuration

| Item | Default |
|------|---------|
| Config file | `./weavegate.toml` (deprecated: `sws.toml`, `edgegate.toml`) |
| Binary | `weavegate` |
| Static root | `./public` |

```sh
weavegate --help
```

## License

This work is primarily distributed under the terms of both the [MIT license](LICENSE-MIT) and the [Apache License (Version 2.0)](LICENSE-APACHE).

© 2019-present [Jose Quintana](https://joseluisq.net)
