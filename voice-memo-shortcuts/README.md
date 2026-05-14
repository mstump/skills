# voice-memo-shortcuts

Two macOS shortcuts — **Start** and **Stop** — for Voice Memos, triggerable from Spotlight or any launcher (Raycast, Sol, Alfred).

## Setup

```bash
chmod +x setup.sh
./setup.sh
```

This builds two `.app` bundles in `~/Applications/` using `osacompile` and indexes them for Spotlight immediately.

**First run:** macOS will prompt for Accessibility permission for each app (once each). Approve in:
`System Settings → Privacy & Security → Accessibility`

## Usage

| Trigger | Command |
|---------|---------|
| Spotlight | `⌘Space` → `Voice Memo - Start` |
| Spotlight | `⌘Space` → `Voice Memo - Stop` |
| Raycast | See below |

## Assigning a global keyboard shortcut

`System Settings → Keyboard → Keyboard Shortcuts → App Shortcuts → +`

- Application: `Other…` → pick `~/Applications/Voice Memo - Start.app`  
- Menu Title: *(leave blank)*  
- Shortcut: your key combo (e.g. `⌃⌥R`)

Repeat for Stop.

## Raycast

Copy the `raycast/` scripts to any directory, then in Raycast:
`Settings → Extensions → Script Commands → Add Directory`

Assign hotkeys directly in the Raycast extension list.

## How it works

- **Start**: opens Voice Memos (or foregrounds it) and sends `⌘N` (New Recording), which creates and immediately begins a recording
- **Stop**: foregrounds Voice Memos and sends `Space`, which stops the active recording

The scripts use `System Events` UI scripting, which requires the one-time Accessibility permission.
