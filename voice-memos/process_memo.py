#!/usr/bin/env python3
"""
Voice Memos Pipeline
Extracts transcript from an Apple Voice Memo, matches it to a Google Calendar
meeting, generates a summary, and writes an Obsidian note with YAML frontmatter.
"""

import json
import os
import re
import struct
import subprocess
import sys
import time
from datetime import datetime, timedelta
from pathlib import Path

import anthropic
import yaml

CONFIG_FILE = Path(__file__).parent / "config.yaml"


# ── Config ────────────────────────────────────────────────────────────────────

def load_config() -> dict:
    with open(CONFIG_FILE) as f:
        return yaml.safe_load(f)


# ── File metadata ─────────────────────────────────────────────────────────────

def get_file_creation_time(path: str) -> datetime:
    stat = os.stat(path)
    ts = getattr(stat, "st_birthtime", stat.st_mtime)
    return datetime.fromtimestamp(ts)


# ── Transcript extraction ─────────────────────────────────────────────────────

def _extract_via_mdls(path: str) -> str | None:
    """Ask Spotlight for kMDItemTextContent — works once the file is indexed."""
    result = subprocess.run(
        ["mdls", "-name", "kMDItemTextContent", path],
        capture_output=True, text=True,
    )
    out = result.stdout.strip()
    if not out or "(null)" in out:
        return None
    match = re.search(r'kMDItemTextContent\s*=\s*"(.*)"', out, re.DOTALL)
    if match:
        return match.group(1).replace("\\n", "\n").strip()
    return None


def _read_atom(data: bytes, offset: int) -> tuple[int, str, bytes]:
    if offset + 8 > len(data):
        return 0, "", b""
    size = struct.unpack_from(">I", data, offset)[0]
    atype = data[offset + 4:offset + 8].decode("latin-1", errors="replace")
    if size == 1:
        if offset + 16 > len(data):
            return 0, "", b""
        size = struct.unpack_from(">Q", data, offset + 8)[0]
        payload = data[offset + 16:offset + size]
    elif size == 0:
        payload = data[offset + 8:]
        size = len(data) - offset
    else:
        payload = data[offset + 8:offset + size]
    return size, atype, payload


def _find_atom(data: bytes, target: str, offset: int = 0) -> bytes | None:
    """Walk the QuickTime atom tree looking for a named atom."""
    pos = offset
    end = len(data)
    while pos < end:
        size, atype, payload = _read_atom(data, pos)
        if size < 8:
            break
        if atype == target:
            return payload
        if atype in ("moov", "trak", "mdia", "minf", "udta", "meta", "ilst"):
            found = _find_atom(payload, target)
            if found is not None:
                return found
        pos += size
    return None


def _extract_via_m4a_atoms(path: str) -> str | None:
    """Parse the m4a binary for known Apple transcript atom names."""
    try:
        data = Path(path).read_bytes()
    except (OSError, PermissionError):
        return None

    for name in ("tran", "©trn", "trns", "tdsc"):
        payload = _find_atom(data, name)
        if payload:
            text = payload.decode("utf-8", errors="replace").strip("\x00").strip()
            if len(text) > 20:
                return text
    return None


def get_transcript(path: str, config: dict) -> str | None:
    """Try mdls then m4a atom parsing, retrying to allow Spotlight to catch up."""
    wait = config.get("transcript_wait_seconds", 30)
    attempts = 3
    delay = max(1, wait // attempts)

    for attempt in range(attempts):
        transcript = _extract_via_mdls(path) or _extract_via_m4a_atoms(path)
        if transcript:
            return transcript
        if attempt < attempts - 1:
            print(f"  waiting for transcript ({attempt + 1}/{attempts - 1})…", flush=True)
            time.sleep(delay)

    return None


# ── Claude API ────────────────────────────────────────────────────────────────

def correct_transcript(raw: str, config: dict) -> str:
    client = anthropic.Anthropic()
    resp = client.messages.create(
        model=config["model"],
        max_tokens=8192,
        messages=[{
            "role": "user",
            "content": (
                "Fix grammar, punctuation, capitalization, and obvious speech-to-text "
                "errors in this transcript. Preserve the speaker's words and meaning. "
                "Return only the corrected transcript with no commentary.\n\n"
                f"TRANSCRIPT:\n{raw}"
            ),
        }],
    )
    return resp.content[0].text.strip()


def generate_note_content(transcript: str, meeting: dict | None, file_time: datetime, config: dict) -> dict:
    client = anthropic.Anthropic()

    meeting_ctx = ""
    if meeting:
        att = ", ".join(str(a) for a in meeting.get("attendees", []))
        meeting_ctx = (
            f"Meeting: {meeting.get('title', '')}\n"
            f"Attendees: {att}\n"
            f"Time: {meeting.get('start_time', '')} – {meeting.get('end_time', '')}\n\n"
        )

    resp = client.messages.create(
        model=config["model"],
        max_tokens=2048,
        messages=[{
            "role": "user",
            "content": (
                "Analyze this meeting transcript. Return a JSON object with:\n"
                "- title: short descriptive title (max 60 chars)\n"
                "- summary: markdown string with 3-5 bullet points (key points, decisions, action items)\n"
                "- tags: array of lowercase kebab-case tag strings\n\n"
                f"{meeting_ctx}"
                f"Recorded: {file_time.strftime('%Y-%m-%d %H:%M')}\n\n"
                f"TRANSCRIPT:\n{transcript}\n\n"
                "Return ONLY the JSON object, no markdown fences."
            ),
        }],
    )

    raw = resp.content[0].text.strip()
    raw = re.sub(r"^```(?:json)?\s*|\s*```$", "", raw, flags=re.MULTILINE).strip()
    return json.loads(raw)


# ── Google Calendar via `claude` CLI ──────────────────────────────────────────

def find_meetings(file_time: datetime, config: dict) -> list[dict]:
    if not config.get("use_claude_calendar", True):
        return []

    import shutil
    if not shutil.which("claude"):
        print("  (claude CLI not found — skipping calendar lookup)")
        return []

    window = config.get("time_window_minutes", 120)
    start = (file_time - timedelta(minutes=window)).strftime("%Y-%m-%dT%H:%M:%S")
    end = (file_time + timedelta(minutes=30)).strftime("%Y-%m-%dT%H:%M:%S")

    prompt = (
        f"Using Google Calendar, list events between {start} and {end} in local time. "
        "Return a JSON array only — no prose. Each element: "
        '{"title": "...", "start_time": "...", "end_time": "...", '
        '"attendees": ["name or email", ...], "description": "..."}. '
        "If no events found, return []."
    )

    try:
        result = subprocess.run(
            [
                "claude", "-p", prompt,
                "--allowedTools",
                "mcp__claude_ai_Google_Calendar__list_events,mcp__claude_ai_Google_Calendar__list_calendars",
            ],
            capture_output=True, text=True, timeout=90,
        )
        match = re.search(r"\[.*\]", result.stdout, re.DOTALL)
        if match:
            return json.loads(match.group())
    except (subprocess.TimeoutExpired, json.JSONDecodeError, FileNotFoundError):
        pass

    return []


# ── User confirmation ─────────────────────────────────────────────────────────

def confirm_meeting(memo_path: str, file_time: datetime, meetings: list[dict], transcript: str) -> dict | None:
    sep = "─" * 62
    print(f"\n{sep}")
    print(f"Memo:     {Path(memo_path).name}")
    print(f"Recorded: {file_time.strftime('%Y-%m-%d %H:%M:%S')}")
    preview = transcript[:200].replace("\n", " ")
    if len(transcript) > 200:
        preview += "…"
    print(f"Preview:  {preview}")

    if meetings:
        print(f"\n{len(meetings)} calendar event(s) found nearby:")
        for i, m in enumerate(meetings, 1):
            atts = m.get("attendees", [])
            att_str = ", ".join(str(a) for a in atts[:4])
            if len(atts) > 4:
                att_str += f" +{len(atts) - 4} more"
            print(f"\n  [{i}] {m.get('title', 'Untitled')}")
            print(f"       {m.get('start_time', '')} – {m.get('end_time', '')}")
            if att_str:
                print(f"       {att_str}")
    else:
        print("\nNo matching calendar events found.")

    n = len(meetings)
    print(f"\n  [{n + 1}] Enter meeting details manually")
    print(f"  [0] Skip")
    print(sep)

    while True:
        raw = input(f"Select [0–{n + 1}]: ").strip()
        if raw.isdigit() and 0 <= int(raw) <= n + 1:
            choice = int(raw)
            break

    if choice == 0:
        return None
    if choice <= n:
        return meetings[choice - 1]

    # Manual entry
    title = input("Meeting title: ").strip() or f"Meeting {file_time.strftime('%Y-%m-%d %H:%M')}"
    att_raw = input("Attendees (comma-separated, or blank): ").strip()
    attendees = [a.strip() for a in att_raw.split(",") if a.strip()]
    start_str = input(f"Start time (default {file_time.strftime('%H:%M')}): ").strip()
    return {
        "title": title,
        "attendees": attendees,
        "start_time": start_str or file_time.strftime("%H:%M"),
        "end_time": "",
    }


# ── Obsidian output ───────────────────────────────────────────────────────────

def write_obsidian_note(
    output_dir: str,
    note_data: dict,
    file_time: datetime,
    meeting: dict | None,
    transcript: str,
) -> Path:
    out = Path(output_dir).expanduser()
    out.mkdir(parents=True, exist_ok=True)

    title = note_data.get("title", f"Meeting {file_time.strftime('%Y-%m-%d %H:%M')}")
    safe_title = re.sub(r'[<>:"/\\|?*]', "", title)[:60].strip()
    filepath = out / f"{file_time.strftime('%Y-%m-%d')} {safe_title}.md"

    fm: dict = {
        "title": title,
        "date": file_time.strftime("%Y-%m-%d"),
        "time": file_time.strftime("%H:%M"),
        "type": "meeting-note",
    }
    if meeting:
        fm["meeting"] = meeting.get("title", "")
        if meeting.get("start_time"):
            fm["meeting_start"] = meeting["start_time"]
        if meeting.get("end_time"):
            fm["meeting_end"] = meeting["end_time"]
        if meeting.get("attendees"):
            fm["attendees"] = meeting["attendees"]
    if note_data.get("tags"):
        fm["tags"] = note_data["tags"]

    lines = ["---"]
    for k, v in fm.items():
        if isinstance(v, list):
            lines.append(f"{k}:")
            for item in v:
                lines.append(f'  - "{item}"')
        else:
            lines.append(f"{k}: {json.dumps(str(v))}")
    lines.append("---\n")

    body = "\n".join(lines)
    body += f"# {title}\n\n"
    body += "## Summary\n\n"
    body += note_data.get("summary", "").strip() + "\n\n"
    body += "## Transcript\n\n"
    body += transcript.strip() + "\n"

    filepath.write_text(body)
    return filepath


# ── Entry point ───────────────────────────────────────────────────────────────

def main():
    if len(sys.argv) < 2:
        print("Usage: process_memo.py <path-to-memo.m4a>", file=sys.stderr)
        sys.exit(1)

    memo_path = sys.argv[1]
    if not Path(memo_path).exists():
        print(f"File not found: {memo_path}", file=sys.stderr)
        sys.exit(1)

    config = load_config()
    print(f"\nProcessing: {Path(memo_path).name}")

    file_time = get_file_creation_time(memo_path)
    print(f"Recorded:  {file_time.strftime('%Y-%m-%d %H:%M:%S')}")

    print("Extracting transcript… ", end="", flush=True)
    transcript = get_transcript(memo_path, config)

    if not transcript:
        print("not found.")
        print(
            "\nCould not extract transcript automatically.\n"
            "Tip: grant Full Disk Access to Terminal in\n"
            "     System Settings → Privacy & Security → Full Disk Access\n"
        )
        manual = input("Paste transcript manually (or Enter to skip): ").strip()
        if not manual:
            sys.exit(0)
        transcript = manual
    else:
        print(f"ok ({len(transcript):,} chars)")

    print("Correcting transcript with Claude… ", end="", flush=True)
    corrected = correct_transcript(transcript, config)
    print("done")

    print("Querying Google Calendar… ", end="", flush=True)
    meetings = find_meetings(file_time, config)
    print(f"{len(meetings)} event(s) found")

    meeting = confirm_meeting(memo_path, file_time, meetings, corrected)
    if meeting is None:
        print("Skipped.")
        sys.exit(0)

    print("\nGenerating summary… ", end="", flush=True)
    note_data = generate_note_content(corrected, meeting, file_time, config)
    print("done")

    output_dir = config.get("output_dir", "~/Documents")
    filepath = write_obsidian_note(output_dir, note_data, file_time, meeting, corrected)
    print(f"\nSaved: {filepath}\n")


if __name__ == "__main__":
    main()
