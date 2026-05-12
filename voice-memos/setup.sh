#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "Installing Python dependencies…"
pip3 install anthropic pyyaml --break-system-packages --quiet

chmod +x "$SCRIPT_DIR/watch.sh"
chmod +x "$SCRIPT_DIR/process_memo.py"

echo ""
echo "Setup complete. Before running:"
echo ""
echo "  1. Edit config.yaml — set output_dir to your Obsidian vault path"
echo "  2. Grant Full Disk Access to Terminal.app:"
echo "       System Settings → Privacy & Security → Full Disk Access"
echo "  3. Set ANTHROPIC_API_KEY in your environment (or ~/.zshrc)"
echo ""
echo "Usage:"
echo "  ./watch.sh                            # watch for new memos (continuous)"
echo "  python3 process_memo.py memo.m4a      # process a single memo"
