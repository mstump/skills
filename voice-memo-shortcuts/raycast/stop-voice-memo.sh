#!/bin/bash

# Required parameters:
# @raycast.schemaVersion 1
# @raycast.title Stop Voice Memo
# @raycast.mode silent

# Optional parameters:
# @raycast.icon ⏹️
# @raycast.packageName Voice Memos
# @raycast.description Stop the current Voice Memos recording

osascript -e 'tell application "Voice Memos" to activate'
sleep 0.4
osascript -e 'tell application "System Events" to tell process "Voice Memos" to keystroke " "'
