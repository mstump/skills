# voice-memos

Watches Apple Voice Memos for new recordings, extracts the transcript, corrects it with Claude, matches it to a Google Calendar meeting, and writes an Obsidian note with YAML frontmatter and a summary.

## Pipeline

```
new .m4a file
    → extract Apple transcript (mdls / m4a atom parsing)
    → correct transcript with Claude
    → find nearby Google Calendar events (via claude CLI + MCP)
    → confirm meeting + attendees with user
    → generate title, summary, tags with Claude
    → write Obsidian note with YAML frontmatter
```

## Setup

```bash
./setup.sh
```

Then:
1. Set `output_dir` in `~/.config/skills/voice-memos.yaml`
2. Grant **Full Disk Access** to `target/release/process-memo`:
   `System Settings → Privacy & Security → Full Disk Access`
3. Ensure you are logged in to Claude Code (`claude`)

## Usage

```bash
# Watch for new memos continuously
./target/release/process-memo watch

# Process a single memo
./target/release/process-memo /path/to/memo.m4a
```

## Configuration

Settings are loaded from `config.yaml` in this directory, then overridden by `~/.config/skills/voice-memos.yaml` if it exists. Put personal settings (paths, model preference) in the user config so they survive repo updates.

```yaml
# ~/.config/skills/voice-memos.yaml
output_dir: "~/Documents/Obsidian/Meeting Notes"
model: "claude-opus-4-7"
```

| Key | Default | Description |
|---|---|---|
| `voice_memos_dir` | `~/Library/Group Containers/…/Recordings` | Where Voice Memos stores `.m4a` files |
| `output_dir` | `~/Documents/Obsidian/Meeting Notes` | Where to write Obsidian notes |
| `model` | `claude-opus-4-7` | Claude model for transcript correction and summarization |
| `time_window_minutes` | `120` | Minutes before recording time to search for calendar events |
| `transcript_wait_seconds` | `30` | How long to wait for Apple's on-device transcription before giving up |
| `use_claude_calendar` | `true` | Set to `false` to skip Google Calendar lookup |

## Output format

Each note is written as `YYYY-MM-DD <Title>.md` with this structure:

```markdown
---
title: "..."
date: 2025-01-15
time: "14:30"
type: meeting-note
meeting: "Team Sync"
meeting_start: "14:00"
meeting_end: "15:00"
attendees:
  - "Alice"
  - "Bob"
tags:
  - engineering
  - planning
---

# Title

## Summary

- Key point one
- Decision made
- Action item: Alice to follow up by Friday

## Transcript

Full corrected transcript…
```

## Running as a Login Item (LaunchAgent)

Running `process-memo watch` as a macOS LaunchAgent starts the pipeline automatically at login and keeps it alive in the background. In this mode there's no terminal, so the meeting-confirmation step uses an `osascript` dialog instead, and success/error events surface as macOS notifications.

### Full Disk Access

Grant Full Disk Access to the binary:

```
System Settings → Privacy & Security → Full Disk Access → click + → target/release/process-memo
```

### Install

```bash
chmod +x setup-launchd.sh
./setup-launchd.sh install
```

This writes a plist to `~/Library/LaunchAgents/com.mstump.voice-memos-pipeline.plist` and loads it immediately — no reboot required.

### Manage

```bash
./setup-launchd.sh status     # check if it's running
./setup-launchd.sh uninstall  # stop and remove the plist
```

```bash
# View live logs
tail -f ~/Library/Logs/voice-memos-pipeline.log
```

### How headless mode differs from terminal mode

| | Terminal | LaunchAgent (headless) |
|---|---|---|
| Meeting confirmation | Text prompt in terminal | `osascript` dialog box |
| No transcript found | Prompts to paste manually | macOS notification, then skips |
| Note saved | Prints path to stdout | macOS notification |

## Troubleshooting

**Transcript not found** — The binary needs Full Disk Access. Without it, `mdls` cannot read Voice Memos metadata. See Setup step 2.

**No calendar events found** — Check that `claude` CLI is in your PATH and the Google Calendar MCP is authenticated. You can set `use_claude_calendar: false` and enter meeting details manually.

**`claude` CLI not found** — Install Claude Code or add it to your PATH.
