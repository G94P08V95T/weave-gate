#!/usr/bin/env bash
# Pack project sources (everything except target/) into an archive under target/.
#
# Usage (from repository root or any path):
#   ./scripts/pack-source.sh
#   ./scripts/pack-source.sh -o target/my-bundle.tar.gz
#
# Environment:
#   PACK_NAME   Archive base name (default: weavegate-source)
#   PACK_FORMAT tar.gz | tar.zst (default: tar.gz)

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

PACK_NAME="${PACK_NAME:-weavegate-source}"
PACK_FORMAT="${PACK_FORMAT:-tar.gz}"

version=""
if [[ -f Cargo.toml ]]; then
    version="$(grep -E '^version\s*=' Cargo.toml | head -1 | sed -E 's/.*"([^"]+)".*/\1/')"
fi
stamp="$(date +%Y%m%d_%H%M%S)"
suffix="${version:+$version-}${stamp}"

output=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        -o | --output)
            output="${2:?missing value for $1}"
            shift 2
            ;;
        -h | --help)
            sed -n '2,12p' "$0"
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            exit 1
            ;;
    esac
done

mkdir -p target

case "$PACK_FORMAT" in
    tar.gz)
        [[ -n "$output" ]] || output="target/${PACK_NAME}-${suffix}.tar.gz"
        tar -czf "$output" \
            --exclude='./target' \
            --exclude='./.git' \
            -C "$ROOT" .
        ;;
    tar.zst)
        if ! command -v zstd >/dev/null 2>&1; then
            echo "zstd is required for PACK_FORMAT=tar.zst" >&2
            exit 1
        fi
        [[ -n "$output" ]] || output="target/${PACK_NAME}-${suffix}.tar.zst"
        tar -cf - \
            --exclude='./target' \
            --exclude='./.git' \
            -C "$ROOT" . | zstd -T0 -o "$output"
        ;;
    *)
        echo "Unsupported PACK_FORMAT: $PACK_FORMAT (use tar.gz or tar.zst)" >&2
        exit 1
        ;;
esac

size="$(du -h "$output" | cut -f1)"
echo "Created: $output ($size)"
