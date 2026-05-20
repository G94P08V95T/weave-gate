#!/usr/bin/env bash
# bench-proxy v2: compare direct upstream vs WeaveGate (Rust/Tokio echo upstream).
#
# Requirements: wrk, curl, cargo (release weavegate + bench-echo)
#
# Usage (from repo root):
#   ./scripts/bench-proxy.sh
#   THREADS_MULTIPLIER=1 WRK_CONNECTIONS=100 ./scripts/bench-proxy.sh
#   WRK_WARMUP=2s ./scripts/bench-proxy.sh

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

WRK_THREADS="${WRK_THREADS:-4}"
WRK_CONNECTIONS="${WRK_CONNECTIONS:-100}"
WRK_DURATION="${WRK_DURATION:-15s}"
WRK_WARMUP="${WRK_WARMUP:-}"
UPSTREAM_PORT="${UPSTREAM_PORT:-13000}"
GATEWAY_PORT="${GATEWAY_PORT:-18787}"
THREADS_MULTIPLIER="${THREADS_MULTIPLIER:-1}"
READY_TIMEOUT_SECS="${READY_TIMEOUT_SECS:-30}"

if ! command -v wrk >/dev/null 2>&1; then
    echo "wrk is required. Install wrk and retry." >&2
    exit 1
fi
if ! command -v curl >/dev/null 2>&1; then
    echo "curl is required for readiness checks." >&2
    exit 1
fi

BIN="${BIN:-$ROOT/target/release/weavegate}"
ECHO_BIN="${ECHO_BIN:-$ROOT/target/release/bench-echo}"
if [[ ! -x "$BIN" ]] || [[ ! -x "$ECHO_BIN" ]]; then
    echo "Building weavegate and bench-echo (release)..."
    cargo build --release -q --bin weavegate --bin bench-echo
fi

BENCH_DIR="$(mktemp -d)"
trap 'rm -rf "$BENCH_DIR"; kill $(jobs -p) 2>/dev/null || true' EXIT

DIRECT_URL="http://127.0.0.1:${UPSTREAM_PORT}/users/1"
VIA_URL="http://127.0.0.1:${GATEWAY_PORT}/api/users/1"

wait_http() {
    local url="$1"
    local name="$2"
    local i=0
    while (( i < READY_TIMEOUT_SECS )); do
        if curl -sf -o /dev/null -m 1 "$url" 2>/dev/null; then
            return 0
        fi
        sleep 0.1
        ((i++)) || true
    done
    echo "timeout waiting for $name at $url" >&2
    return 1
}

BENCH_ECHO_PORT="$UPSTREAM_PORT" "$ECHO_BIN" &
ECHO_PID=$!
wait_http "$DIRECT_URL" "bench-echo upstream"

cat >"$BENCH_DIR/weavegate.toml" <<EOF
[general]
host = "127.0.0.1"
port = $GATEWAY_PORT
root = "$ROOT/docker/public"
log-level = "error"
threads-multiplier = $THREADS_MULTIPLIER
compression = false

[advanced]
proxy-pool-max-idle-per-host = 32
proxy-pool-idle-timeout-secs = 90
proxy-first = true

[[advanced.proxies]]
name = "bench-api"
source = "/api/**"
target = "http://127.0.0.1:$UPSTREAM_PORT"
strip-prefix = "/api"
EOF

"$BIN" -w "$BENCH_DIR/weavegate.toml" &
WG_PID=$!
wait_http "$VIA_URL" "weavegate"

run_wrk() {
    local label="$1"
    local url="$2"
    local outfile="$BENCH_DIR/${label// /_}.txt"
    echo ""
    echo "=== $label ==="
    echo "URL: $url"
    wrk -t"$WRK_THREADS" -c"$WRK_CONNECTIONS" -d"$WRK_DURATION" --latency "$url" 2>&1 | tee "$outfile"
}

if [[ -n "$WRK_WARMUP" ]]; then
    echo "Warmup (${WRK_WARMUP})..."
    wrk -t"$WRK_THREADS" -c"$WRK_CONNECTIONS" -d"$WRK_WARMUP" "$DIRECT_URL" >/dev/null 2>&1 || true
    wrk -t"$WRK_THREADS" -c"$WRK_CONNECTIONS" -d"$WRK_WARMUP" "$VIA_URL" >/dev/null 2>&1 || true
fi

run_wrk "direct-upstream" "$DIRECT_URL"
run_wrk "via-weavegate" "$VIA_URL"

parse_metric() {
    local file="$1"
    local key="$2"
    grep -E "$key" "$file" 2>/dev/null | tail -1 | awk '{print $NF}' | tr -d '\r'
}

DIRECT_RPS="$(parse_metric "$BENCH_DIR/direct-upstream.txt" 'Requests/sec:')"
VIA_RPS="$(parse_metric "$BENCH_DIR/via-weavegate.txt" 'Requests/sec:')"
DIRECT_P50="$(parse_metric "$BENCH_DIR/direct-upstream.txt" '50%')"
VIA_P50="$(parse_metric "$BENCH_DIR/via-weavegate.txt" '50%')"
DIRECT_P99="$(parse_metric "$BENCH_DIR/direct-upstream.txt" '99%')"
VIA_P99="$(parse_metric "$BENCH_DIR/via-weavegate.txt" '99%')"
NON2XX="$(grep -E 'Non-2xx or 3xx' "$BENCH_DIR/via-weavegate.txt" 2>/dev/null | tail -1 | awk '{print $NF}' | tr -d '\r' || true)"
NON2XX="${NON2XX:-0}"

echo ""
echo "========== Summary (bench-proxy v2) =========="
echo "Machine: $(uname -n) | wrk: -t${WRK_THREADS} -c${WRK_CONNECTIONS} -d${WRK_DURATION} | threads-multiplier=${THREADS_MULTIPLIER}"
echo "Upstream: bench-echo (Tokio/Hyper) on port ${UPSTREAM_PORT}"
echo ""
printf "  %-18s %12s %10s %10s\n" "Scenario" "RPS" "P50" "P99"
printf "  %-18s %12s %10s %10s\n" "direct-upstream" "$DIRECT_RPS" "$DIRECT_P50" "$DIRECT_P99"
printf "  %-18s %12s %10s %10s\n" "via-weavegate" "$VIA_RPS" "$VIA_P50" "$VIA_P99"

if [[ -n "$DIRECT_RPS" && -n "$VIA_RPS" ]] && awk -v d="$DIRECT_RPS" 'BEGIN { exit (d > 0) ? 0 : 1 }'; then
    RATIO="$(awk -v v="$VIA_RPS" -v d="$DIRECT_RPS" 'BEGIN { printf "%.3f", v / d }')"
    echo ""
    echo "  RPS ratio (via / direct): ${RATIO}"
    awk -v r="$RATIO" 'BEGIN {
        if (r >= 0.85) exit 0;
        print "  WARNING: ratio < 0.85 (plan target). Tune pool size or threads-multiplier.";
        exit 0;
    }'
fi

if [[ "$NON2XX" != "0" && -n "$NON2XX" ]]; then
    echo "  WARNING: via-weavegate had Non-2xx/3xx responses: ${NON2XX}"
elif [[ "$NON2XX" == "0" ]]; then
    echo "  Non-2xx via-weavegate: 0 (OK)"
fi

echo ""
echo "Full wrk output: $BENCH_DIR"

kill "$WG_PID" 2>/dev/null || true
kill "$ECHO_PID" 2>/dev/null || true
wait "$WG_PID" 2>/dev/null || true
wait "$ECHO_PID" 2>/dev/null || true
