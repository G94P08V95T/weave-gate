# WeaveGate Routes Bootstrap API (v1)

WeaveGate can fetch reverse-proxy rules from a control-plane HTTP API at startup.
This document defines the **v1 contract** that backends must implement.

## Request

```http
GET {routes-bootstrap-url}?client={client-id}&weavegate={version}
Accept: application/json
Authorization: Bearer <token>
User-Agent: weavegate/{version}
```

| Query parameter | Source | Description |
|-----------------|--------|-------------|
| `client` | TOML `client-id` (optional) | Client profile, e.g. `desktop` |
| `weavegate` | Binary version | WeaveGate release version |

`Authorization` is sent only when `token-env` is set in `weavegate.toml`; the token value is read from that environment variable (never stored in the config file).

The bootstrap URL may use `https://` (recommended in production). WeaveGate validates TLS using the platform WebPKI root store (rustls).

## Upstream targets (HTTP and HTTPS)

`target` / `targets` in routes JSON (and TOML `[[advanced.proxies]]`) may use `http://` or `https://`. HTTPS upstreams use the same rustls trust store as the bootstrap client. WebSocket and SSE tunnels over `wss://` upstreams are supported when the path matches a rule.

### mkcert (local / intranet HTTPS)

For certificates signed by [mkcert](https://github.com/FiloSottile/mkcert), point WeaveGate at the mkcert **root CA** PEM (not the per-site cert):

```bash
mkcert -install
mkcert -CAROOT   # e.g. ~/.local/share/mkcert
```

```toml
[advanced.proxy-tls]
# Linux example; use the path from `mkcert -CAROOT`
ca-file = "~/.local/share/mkcert/rootCA.pem"
# default false: trust mkcert CA **and** public WebPKI (corp APIs + local dev)
disable-webpki-roots = false
```

| Option | Meaning |
|--------|---------|
| `ca-file` | Single PEM file (mkcert `rootCA.pem`) |
| `ca-files` | Extra PEM CAs merged into the store |
| `disable-webpki-roots` | `true` = **only** trust listed CAs (pure intranet) |

Applies to **all** HTTPS outbound connections: reverse proxy upstreams and routes bootstrap `url`.

## Response

`Content-Type: application/json`, HTTP `200`.

```json
{
  "version": 1,
  "apps": [
    {
      "id": "app1",
      "routes": [
        {
          "name": "user-service",
          "source": "/app1/api/users/**",
          "target": "https://user-svc.corp.internal",
          "strip-prefix": "/app1/api/users"
        },
        {
          "name": "api-gateway",
          "source": "/app1/api/**",
          "target": "https://gateway.corp.internal",
          "strip-prefix": "/app1/api"
        }
      ]
    }
  ]
}
```

### Fields

| Field | Required | Description |
|-------|----------|-------------|
| `version` | yes | Must be `1` |
| `apps` | yes | List of applications (may be empty) |
| `apps[].id` | yes | Application id (matches static dir `root/{id}/`) |
| `apps[].routes` | yes | Proxy rules for that app (may be empty) |
| `routes[].source` | yes | Glob matched against request path (same as TOML `source`) |
| `routes[].target` | one of | Single upstream base URL |
| `routes[].targets` | one of | Multiple upstreams for round-robin |
| `routes[].strip-prefix` | no | Path prefix removed before forwarding |
| `routes[].name` | no | Service name for logs |
| `routes[].host` | no | Optional `Host` header filter |

`target` and `targets` are mutually exclusive (same rules as `[[advanced.proxies]]` in TOML).

## A + B routing (direct microservices + API gateway)

For one application, combine:

1. **A** — Specific paths to individual services, e.g. `/app1/api/users/**` → user service.
2. **B** — Fallback `/app1/api/**` → corporate API gateway / BFF.

WeaveGate sorts rules by path specificity at startup (longer paths, fewer `*` wildcards first).
The control plane should still return specific routes before the catch-all when possible.

## Failure handling (WeaveGate side)

Configured via `[advanced.routes-bootstrap] on-failure`:

| Value | Behavior |
|-------|----------|
| `cache-then-file` (default) | HTTP failure → read `cache-path` snapshot → fall back to TOML `[[advanced.proxies]]` |
| `fail` | Abort startup if no successful fetch and no usable cache/file rules |
| `file-only` | Skip HTTP; use TOML rules only |

On a successful fetch, WeaveGate writes the raw JSON response to `cache-path` (atomic replace).

## Security notes

- Prefer `https://` for the bootstrap URL and upstream `target` URLs in production (both are supported by WeaveGate).
- Do not embed secrets in route JSON; use short-lived tokens via `token-env`.
- Reject paths containing `..` in `source`.

## Example `weavegate.toml`

```toml
[advanced.routes-bootstrap]
url = "https://control.example.com/api/v1/weavegate/routes"
token-env = "WEAVEGATE_ROUTES_TOKEN"
timeout-secs = 5
cache-path = "./routes.cache.json"
on-failure = "cache-then-file"
client-id = "desktop"

[[advanced.proxies]]
name = "local-override"
source = "/debug/**"
target = "http://127.0.0.1:9999"
```
