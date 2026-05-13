#!/usr/bin/env bash
# Watches the Voice Memos directory and processes new .m4a files automatically.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# LaunchAgents don't inherit shell environment — load secrets from .env if present
if [[ -f "$SCRIPT_DIR/.env" ]]; then
    set -a
    # shellcheck disable=SC1091
    source "$SCRIPT_DIR/.env"
    set +a
fi

VOICE_MEMOS_DIR=$(python3 - <<'EOF'
import yaml, os, sys
with open(sys.argv[1]) as f:
    c = yaml.safe_load(f)
print(os.path.expanduser(c.get("voice_memos_dir", "")))
EOF
"$SCRIPT_DIR/config.yaml")

if [[ -z "$VOICE_MEMOS_DIR" ]]; then
    echo "Error: voice_memos_dir not configured in config.yaml" >&2
    exit 1
fi

if [[ ! -d "$VOICE_MEMOS_DIR" ]]; then
    echo "Error: directory not accessible: $VOICE_MEMOS_DIR" >&2
    echo ""
    echo "Terminal needs Full Disk Access to read Voice Memos:"
    echo "  System Settings → Privacy & Security → Full Disk Access → add Terminal"
    exit 1
fi

echo "Watching: $VOICE_MEMOS_DIR"
echo "Press Ctrl+C to stop."
echo ""

fswatch -0 --event Created "$VOICE_MEMOS_DIR" | while IFS= read -r -d '' file; do
    if [[ "$file" == *.m4a ]]; then
        echo "[$(date '+%H:%M:%S')] New memo: $(basename "$file")"
        # Small delay — let the file finish writing and Apple start transcribing
        sleep 3
        python3 "$SCRIPT_DIR/process_memo.py" "$file" || {
            echo "Error processing $file" >&2
        }
    fi
done
