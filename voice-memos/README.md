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
1. Edit `config.yaml` — set `output_dir` to your Obsidian vault path
2. Grant **Full Disk Access** to Terminal.app:
   `System Settings → Privacy & Security → Full Disk Access`
3. Ensure `ANTHROPIC_API_KEY` is set in your environment

## Usage

```bash
# Watch for new memos continuously
./watch.sh

# Process a single memo
python3 process_memo.py /path/to/memo.m4a
```

## Configuration

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

## Troubleshooting

**Transcript not found** — Terminal needs Full Disk Access. Without it, `mdls` cannot read Voice Memos metadata. See Setup step 2.

**No calendar events found** — Check that `claude` CLI is in your PATH and the Google Calendar MCP is authenticated. You can set `use_claude_calendar: false` and enter meeting details manually.

**`claude` CLI not found** — Install Claude Code or add it to your PATH.
