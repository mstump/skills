#!/usr/bin/env bash
# Creates two macOS .app bundles in ~/Applications/ that Spotlight (and any
# launcher) can find:
#   "Voice Memo - Start"  -- opens Voice Memos and begins a new recording
#   "Voice Memo - Stop"   -- brings Voice Memos to front and stops the recording
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_DIR="${HOME}/Applications"

mkdir -p "${APP_DIR}"

echo "Building Voice Memo apps in ${APP_DIR}..."

# -- Start Recording ----------------------------------------------------------

osacompile -o "${APP_DIR}/Voice Memo - Start.app" - <<'APPLESCRIPT'
-- Open (or foreground) Voice Memos
tell application "Voice Memos" to activate
delay 1.2
-- Cmd+N creates a new recording and starts it immediately
tell application "System Events"
    tell process "Voice Memos"
        set frontmost to true
        keystroke "n" using {command down}
    end tell
end tell
APPLESCRIPT

# -- Stop Recording -----------------------------------------------------------

osacompile -o "${APP_DIR}/Voice Memo - Stop.app" - <<'APPLESCRIPT'
-- Bring Voice Memos to front
tell application "Voice Memos" to activate
delay 0.4
-- Space stops the current recording
tell application "System Events"
    tell process "Voice Memos"
        set frontmost to true
        keystroke " "
    end tell
end tell
APPLESCRIPT

echo "Built: Voice Memo - Start.app"
echo "Built: Voice Memo - Stop.app"

# -- Spotlight indexing -------------------------------------------------------

echo "Indexing for Spotlight..."
mdimport "${APP_DIR}"

# -- Raycast (optional) -------------------------------------------------------

if [[ -d "${HOME}/Library/Application Support/Raycast" ]]; then
    RAYCAST_SCRIPTS="${HOME}/raycast-scripts"
    mkdir -p "${RAYCAST_SCRIPTS}"
    cp "${SCRIPT_DIR}/raycast/"*.sh "${RAYCAST_SCRIPTS}/"
    chmod +x "${RAYCAST_SCRIPTS}/"*.sh
    echo ""
    echo "Raycast detected. Scripts copied to ${RAYCAST_SCRIPTS}"
    echo "In Raycast: Settings -> Extensions -> Script Commands -> Add Directory -> ${RAYCAST_SCRIPTS}"
fi

# -- Done ---------------------------------------------------------------------

echo ""
echo "Done. Trigger via Spotlight: Cmd+Space -> 'Voice Memo - Start' or 'Voice Memo - Stop'"
echo ""
echo "First run only: approve Accessibility access for each app when macOS prompts."
echo "  System Settings -> Privacy & Security -> Accessibility"
echo ""
echo "To assign global keyboard shortcuts:"
echo "  System Settings -> Keyboard -> Keyboard Shortcuts -> App Shortcuts -> +"
echo "  Application: Other -> ${APP_DIR}/Voice Memo - Start.app"
echo "  Menu Title: (leave blank) -> assign your key combo"
