#!/usr/bin/env bash
# Install or uninstall the voice-memos-pipeline LaunchAgent.
#
# Usage:
#   ./setup-launchd.sh install    — write plist and load it
#   ./setup-launchd.sh uninstall  — unload and remove plist
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LABEL="com.mstump.voice-memos-pipeline"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
LOG="$HOME/Library/Logs/voice-memos-pipeline.log"

install() {
    if [[ ! -f "$SCRIPT_DIR/.env" ]]; then
        echo "Error: $SCRIPT_DIR/.env not found."
        echo "Copy .env.example to .env and fill in your ANTHROPIC_API_KEY."
        exit 1
    fi

    mkdir -p "$HOME/Library/LaunchAgents"

    cat > "$PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>$LABEL</string>

    <key>ProgramArguments</key>
    <array>
        <string>$SCRIPT_DIR/target/release/process-memo</string>
        <string>watch</string>
    </array>

    <!-- Start at login and restart automatically if it exits -->
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>

    <!-- Homebrew + system binaries must be on PATH -->
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
        <key>HOME</key>
        <string>$HOME</string>
    </dict>

    <key>StandardOutPath</key>
    <string>$LOG</string>
    <key>StandardErrorPath</key>
    <string>$LOG</string>
</dict>
</plist>
PLIST

    # Load into launchd (works on macOS 10.10+)
    launchctl load "$PLIST"
    echo "Installed and started: $LABEL"
    echo "Logs: $LOG"
}

uninstall() {
    if launchctl list | grep -q "$LABEL"; then
        launchctl unload "$PLIST"
        echo "Stopped: $LABEL"
    fi
    if [[ -f "$PLIST" ]]; then
        rm "$PLIST"
        echo "Removed: $PLIST"
    else
        echo "Plist not found — nothing to remove."
    fi
}

status() {
    if launchctl list | grep -q "$LABEL"; then
        echo "Running"
        launchctl list "$LABEL"
    else
        echo "Not running"
    fi
}

case "${1:-}" in
    install)   install ;;
    uninstall) uninstall ;;
    status)    status ;;
    *)
        echo "Usage: $0 {install|uninstall|status}"
        exit 1
        ;;
esac
