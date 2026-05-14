#!/bin/bash

# Required parameters:
# @raycast.schemaVersion 1
# @raycast.title Start Voice Memo
# @raycast.mode silent

# Optional parameters:
# @raycast.icon 🎙️
# @raycast.packageName Voice Memos
# @raycast.description Open Voice Memos and start a new recording

open -a "Voice Memos"
sleep 1.2
osascript -e 'tell application "System Events" to tell process "Voice Memos" to keystroke "n" using {command down}'
