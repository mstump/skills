#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "Building process-memo binary…"
cargo build --release --manifest-path "$SCRIPT_DIR/Cargo.toml"

echo ""
echo "Setup complete. Before running:"
echo ""
echo "  1. Set output_dir in ~/.config/skills/voice-memos.yaml"
echo "  2. Grant Full Disk Access to target/release/process-memo:"
echo "       System Settings → Privacy & Security → Full Disk Access"
echo "  3. Ensure you are logged in to Claude Code (run: claude)"
echo ""
echo "Usage:"
echo "  ./target/release/process-memo watch                        # watch for new memos"
echo "  ./target/release/process-memo run memo.m4a                 # process a single memo"
echo "  ./target/release/process-memo backfill                     # process all existing memos"
echo "  ./target/release/process-memo backfill --dry-run --keep    # dry run without deleting"
