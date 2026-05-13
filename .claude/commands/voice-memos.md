Process all pending Voice Memos interactively and save them as Obsidian notes.

## Setup

Find the binary:
```bash
SKILLS=$(git -C "$(dirname "$(realpath "$0")")" rev-parse --show-toplevel 2>/dev/null || echo "$HOME/src/skills")
BINARY="$SKILLS/voice-memos/target/release/process-memo"
```

If the binary doesn't exist, run `cargo build --release` in `$SKILLS/voice-memos/` first.

## Workflow

1. **List pending memos**
   ```bash
   MEMO_DIR=$("$BINARY" print-watch-dir)
   ls "$MEMO_DIR"/*.m4a 2>/dev/null
   ```
   If none, tell the user there are no pending memos and stop.

2. **For each `.m4a` file**, run prepare and save the JSON:
   ```bash
   TMPFILE=$(mktemp /tmp/voice-memo-XXXXXX.json)
   "$BINARY" prepare "$MEMO_FILE" > "$TMPFILE"
   ```
   Parse the JSON. If prepare fails (no transcript), note it and continue to the next file.

3. **Present meeting candidates** to the user in this format:

   ---
   **📼 `<filename>` — Recorded `<recorded datetime>`**

   > `<first 200 chars of transcript>…`

   Meeting candidates:
   | # | Meeting | Time | Duration | Attendees | Score | Reason |
   |---|---------|------|----------|-----------|-------|--------|
   | 1 | Team Sync | 12:30–13:00 | 30 min | Alice, Bob | **85** ← recommended | matches sprint discussion |
   | 2 | 1:1 with Manager | 11:00–11:30 | 30 min | Manager | 20 | no topic overlap |

   > Recommendation: **[1] Team Sync** (score 85)
   > Select a meeting [1–N], or 0 for no meeting:

   ---

4. **Get user's selection.** Accept:
   - A number 1–N → use that meeting (0-based index = number - 1)
   - `0` or `none` → save without meeting metadata
   - Enter / blank → accept the recommendation

5. **Finalize the note:**
   ```bash
   # If user selected meeting n (1-based), convert to 0-based index:
   "$BINARY" finalize "$TMPFILE" --select $((n - 1))

   # If no meeting:
   "$BINARY" finalize "$TMPFILE"
   ```
   Report the saved note path. Clean up: `rm "$TMPFILE"`.

6. **After all memos**, give a summary: X notes saved, Y skipped/failed.

## Notes

- `prepare` outputs a JSON object with fields: `memo_path`, `recorded` (ISO 8601), `transcript`, `meetings` (array of scored meetings), `recommended_index`
- Each meeting in the array has: `title`, `start_time`, `end_time`, `duration_minutes`, `attendees`, `relevance_score` (0–100), `relevance_reason`
- `finalize` prints the saved note path to stdout on success
- Source `.m4a` files are deleted after finalization (the binary handles this)
- If a memo has no meeting candidates, present "0 – no meeting" as the only option and auto-confirm
