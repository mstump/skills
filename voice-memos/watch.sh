#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BINARY="$SCRIPT_DIR/target/release/process_memo"

if [[ ! -x "$BINARY" ]]; then
    echo "Error: binary not found — run ./setup.sh first" >&2
    exit 1
fi

# LaunchAgents don't inherit shell environment — load secrets from .env if present
if [[ -f "$SCRIPT_DIR/.env" ]]; then
    set -a
    # shellcheck disable=SC1091
    source "$SCRIPT_DIR/.env"
    set +a
fi

exec "$BINARY" watch
