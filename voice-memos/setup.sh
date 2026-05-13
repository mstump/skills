#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "Building process_memo binary…"
cargo build --release --manifest-path "$SCRIPT_DIR/Cargo.toml"

chmod +x "$SCRIPT_DIR/watch.sh"

echo ""
echo "Setup complete. Before running:"
echo ""
echo "  1. Set output_dir in ~/.config/skills/voice-memos.yaml"
echo "  2. Grant Full Disk Access to Terminal.app:"
echo "       System Settings → Privacy & Security → Full Disk Access"
echo "  3. Set ANTHROPIC_API_KEY in your environment (or ~/.zshrc)"
echo ""
echo "Usage:"
echo "  ./watch.sh                                        # watch for new memos (continuous)"
echo "  ./target/release/process_memo memo.m4a            # process a single memo"
