#!/usr/bin/env bash
# Minimal mock control plane for manual routes-bootstrap testing.
# Usage: ./scripts/mock-routes-server.sh [port]
# Then point weavegate.toml [advanced.routes-bootstrap] url to http://127.0.0.1:PORT/routes

set -euo pipefail
PORT="${1:-19090}"
BODY='{"version":1,"apps":[{"id":"app1","routes":[{"name":"user-service","source":"/app1/api/users/**","target":"http://127.0.0.1:3001","strip-prefix":"/app1/api/users"},{"name":"api-gateway","source":"/app1/api/**","target":"http://127.0.0.1:3000","strip-prefix":"/app1/api"}]}]}'

echo "Serving v1 routes JSON on http://127.0.0.1:${PORT}/routes"
while true; do
  printf 'HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: %s\r\nConnection: close\r\n\r\n%s' \
    "${#BODY}" "$BODY" | nc -l "127.0.0.1" "$PORT" -q 1 2>/dev/null || true
done
