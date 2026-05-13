use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Local, TimeZone};
use is_terminal::IsTerminal;
use notify::{EventKind, RecursiveMode, Watcher};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

const DEFAULT_CONFIG: &str = include_str!("../config.yaml");
const USER_CONFIG_SUBPATH: &str = ".config/skills/voice-memos.yaml";

// ── Config ────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct Config {
    voice_memos_dir: String,
    output_dir: String,
    model: String,
    time_window_minutes: i64,
    transcript_wait_seconds: u64,
    use_claude_calendar: bool,
}

fn load_config() -> Result<Config> {
    let mut base: serde_yaml::Value =
        serde_yaml::from_str(DEFAULT_CONFIG).context("parsing bundled config.yaml")?;

    if let Some(home) = dirs::home_dir() {
        let user_path = home.join(USER_CONFIG_SUBPATH);
        if user_path.exists() {
            let content = std::fs::read_to_string(&user_path)
                .with_context(|| format!("reading {}", user_path.display()))?;
            if let (
                Ok(serde_yaml::Value::Mapping(overlay)),
                serde_yaml::Value::Mapping(ref mut base_map),
            ) = (serde_yaml::from_str(&content), &mut base)
            {
                for (k, v) in overlay {
                    base_map.insert(k, v);
                }
            }
        }
    }

    serde_yaml::from_value(base).context("deserializing config")
}

// ── File metadata ─────────────────────────────────────────────────────────────

fn get_file_creation_time(path: &Path) -> DateTime<Local> {
    let meta = std::fs::metadata(path).expect("stat failed");
    let sys_time = meta
        .created()
        .or_else(|_| meta.modified())
        .unwrap_or(SystemTime::now());
    let secs = sys_time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    Local
        .timestamp_opt(secs, 0)
        .single()
        .unwrap_or_else(Local::now)
}

// ── Transcript extraction ─────────────────────────────────────────────────────

fn extract_via_mdls(path: &Path) -> Option<String> {
    let output = Command::new("mdls")
        .args(["-name", "kMDItemTextContent", path.to_str()?])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim();
    if text.is_empty() || text.contains("(null)") {
        return None;
    }
    let re = Regex::new(r#"(?s)kMDItemTextContent\s*=\s*"(.*)""#).ok()?;
    let cap = re.captures(text)?;
    let content = cap[1].replace("\\n", "\n").trim().to_string();
    if content.is_empty() {
        None
    } else {
        Some(content)
    }
}

fn read_atom(data: &[u8], offset: usize) -> Option<(usize, [u8; 4], &[u8])> {
    if offset + 8 > data.len() {
        return None;
    }
    let raw_size = u32::from_be_bytes(data[offset..offset + 4].try_into().ok()?) as usize;
    let atype: [u8; 4] = data[offset + 4..offset + 8].try_into().ok()?;
    let (size, payload) = if raw_size == 1 {
        if offset + 16 > data.len() {
            return None;
        }
        let s = u64::from_be_bytes(data[offset + 8..offset + 16].try_into().ok()?) as usize;
        (s, &data[offset + 16..data.len().min(offset + s)])
    } else if raw_size == 0 {
        let s = data.len() - offset;
        (s, &data[offset + 8..])
    } else {
        if offset + raw_size > data.len() {
            return None;
        }
        (raw_size, &data[offset + 8..offset + raw_size])
    };
    if size < 8 {
        return None;
    }
    Some((size, atype, payload))
}

fn find_atom<'a>(data: &'a [u8], target: &[u8; 4]) -> Option<&'a [u8]> {
    const CONTAINERS: &[[u8; 4]] = &[
        *b"moov", *b"trak", *b"mdia", *b"minf", *b"udta", *b"meta", *b"ilst",
    ];
    let mut pos = 0;
    while pos < data.len() {
        let (size, atype, payload) = read_atom(data, pos)?;
        if &atype == target {
            return Some(payload);
        }
        if CONTAINERS.contains(&atype) {
            if let Some(found) = find_atom(payload, target) {
                return Some(found);
            }
        }
        pos += size;
    }
    None
}

fn extract_via_m4a_atoms(path: &Path) -> Option<String> {
    let data = std::fs::read(path).ok()?;
    // ©trn is latin-1 0xA9 followed by "trn"
    const TARGETS: &[[u8; 4]] = &[
        *b"tran",
        [0xa9, b't', b'r', b'n'],
        *b"trns",
        *b"tdsc",
    ];
    for target in TARGETS {
        if let Some(payload) = find_atom(&data, target) {
            let text = String::from_utf8_lossy(payload)
                .trim_matches('\0')
                .trim()
                .to_string();
            if text.len() > 20 {
                return Some(text);
            }
        }
    }
    None
}

fn get_transcript(path: &Path, config: &Config) -> Option<String> {
    let attempts = 3u32;
    let delay = Duration::from_secs((config.transcript_wait_seconds / attempts as u64).max(1));
    for attempt in 0..attempts {
        let t = extract_via_mdls(path).or_else(|| extract_via_m4a_atoms(path));
        if t.is_some() {
            return t;
        }
        if attempt < attempts - 1 {
            println!("  waiting for transcript ({}/{})…", attempt + 1, attempts - 1);
            thread::sleep(delay);
        }
    }
    None
}

// ── Claude API ────────────────────────────────────────────────────────────────

fn call_claude(model: &str, prompt: &str) -> Result<String> {
    let mut child = Command::new("claude")
        .args(["-p", prompt, "--model", model])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("claude CLI not found — install Claude Code from https://claude.ai/code")?;

    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                return Err(anyhow!("claude CLI timed out after 180s"));
            }
            Ok(None) => thread::sleep(Duration::from_millis(200)),
            Err(e) => return Err(e.into()),
        }
    }

    let output = child.wait_with_output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("claude CLI failed: {stderr}"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn correct_transcript(raw: &str, config: &Config) -> Result<String> {
    call_claude(
        &config.model,
        &format!(
            "Fix grammar, punctuation, capitalization, and obvious speech-to-text \
            errors in this transcript. Preserve the speaker's words and meaning. \
            Return only the corrected transcript with no commentary.\n\nTRANSCRIPT:\n{raw}"
        ),
    )
}

#[derive(Debug, Deserialize)]
struct NoteData {
    title: String,
    summary: String,
    tags: Vec<String>,
}

fn generate_note_content(
    transcript: &str,
    meeting: Option<&Meeting>,
    file_time: &DateTime<Local>,
    config: &Config,
) -> Result<NoteData> {
    let meeting_ctx = meeting
        .map(|m| {
            format!(
                "Meeting: {}\nAttendees: {}\nTime: {} – {}\n\n",
                m.title,
                m.attendees.join(", "),
                m.start_time,
                m.end_time,
            )
        })
        .unwrap_or_default();

    let prompt = format!(
        "Analyze this meeting transcript. Return a JSON object with:\n\
        - title: short descriptive title (max 60 chars)\n\
        - summary: markdown string with 3-5 bullet points (key points, decisions, action items)\n\
        - tags: array of lowercase kebab-case tag strings\n\n\
        {meeting_ctx}\
        Recorded: {}\n\nTRANSCRIPT:\n{transcript}\n\n\
        Return ONLY the JSON object, no markdown fences.",
        file_time.format("%Y-%m-%d %H:%M")
    );

    let raw = call_claude(&config.model, &prompt)?;
    let re = Regex::new(r"(?s)^```(?:json)?\s*|\s*```$").unwrap();
    let cleaned = re.replace_all(raw.trim(), "").trim().to_string();
    serde_json::from_str(&cleaned).with_context(|| format!("parsing note JSON:\n{cleaned}"))
}

// ── Google Calendar via `claude` CLI ──────────────────────────────────────────

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct Meeting {
    #[serde(default)]
    title: String,
    #[serde(default)]
    start_time: String,
    #[serde(default)]
    end_time: String,
    #[serde(default)]
    attendees: Vec<String>,
    #[serde(default)]
    description: Option<String>,
}

fn find_meetings(file_time: &DateTime<Local>, config: &Config) -> Vec<Meeting> {
    if !config.use_claude_calendar {
        return vec![];
    }

    let start = (*file_time - chrono::Duration::minutes(config.time_window_minutes))
        .format("%Y-%m-%dT%H:%M:%S");
    let end = (*file_time + chrono::Duration::minutes(30)).format("%Y-%m-%dT%H:%M:%S");

    let prompt = format!(
        "Using Google Calendar, list events between {start} and {end} in local time. \
        Return a JSON array only — no prose. Each element: \
        {{\"title\": \"...\", \"start_time\": \"...\", \"end_time\": \"...\", \
        \"attendees\": [\"name or email\", ...], \"description\": \"...\"}}. \
        If no events found, return []."
    );

    let mut child = match Command::new("claude")
        .args([
            "-p",
            &prompt,
            "--allowedTools",
            "mcp__claude_ai_Google_Calendar__list_events,mcp__claude_ai_Google_Calendar__list_calendars",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => {
            println!("  (claude CLI not found — skipping calendar lookup)");
            return vec![];
        }
    };

    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                return vec![];
            }
            Ok(None) => thread::sleep(Duration::from_millis(200)),
            Err(_) => return vec![],
        }
    }

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(_) => return vec![],
    };

    let text = String::from_utf8_lossy(&output.stdout);
    let re = Regex::new(r"(?s)\[.*\]").unwrap();
    re.find(&text)
        .and_then(|m| serde_json::from_str(m.as_str()).ok())
        .unwrap_or_default()
}

// ── User confirmation ─────────────────────────────────────────────────────────

fn osascript(script: &str) -> String {
    Command::new("osascript")
        .args(["-e", script])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn notify(message: &str) {
    let safe = message.replace('\\', "\\\\").replace('"', "\\\"");
    osascript(&format!(
        "display notification \"{safe}\" with title \"Voice Memos Pipeline\""
    ));
}

fn escape_as(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn confirm_meeting_gui(file_time: &DateTime<Local>, meetings: &[Meeting]) -> Option<Meeting> {
    let time_str = file_time.format("%Y-%m-%d %H:%M").to_string();

    if meetings.is_empty() {
        let result = osascript(&format!(
            "display dialog \"Voice Memo recorded at {time_str}\\n\\n\
            No calendar events found nearby. Continue without meeting metadata?\" \
            buttons {{\"Skip\", \"Continue\"}} default button \"Continue\""
        ));
        if result.contains("Skip") || result.is_empty() {
            return None;
        }
        return Some(Meeting {
            title: format!("Recording {time_str}"),
            ..Default::default()
        });
    }

    let options: Vec<String> = meetings
        .iter()
        .map(|m| {
            let start = if m.start_time.len() >= 5 {
                &m.start_time[..5]
            } else {
                &m.start_time
            };
            format!("{} ({start})", m.title)
        })
        .collect();

    let skip = "None of these / skip";
    let as_list = options
        .iter()
        .map(|o| format!("\"{}\"", escape_as(o)))
        .chain(std::iter::once(format!("\"{skip}\"")))
        .collect::<Vec<_>>()
        .join(", ");

    let result = osascript(&format!(
        "choose from list {{{as_list}}} \
        with prompt \"Voice Memo — {time_str}\\nWhich meeting does this belong to?\" \
        default items {{\"{}\"}}",
        escape_as(&options[0])
    ));

    if result.is_empty() || result == "false" || result == skip {
        return None;
    }

    options
        .iter()
        .position(|o| o == &result)
        .and_then(|i| meetings.get(i))
        .cloned()
}

fn prompt_line(p: &str) -> String {
    print!("{p}");
    io::stdout().flush().ok();
    let mut line = String::new();
    io::stdin().read_line(&mut line).ok();
    line.trim().to_string()
}

fn confirm_meeting(
    memo_path: &Path,
    file_time: &DateTime<Local>,
    meetings: Vec<Meeting>,
    transcript: &str,
) -> Option<Meeting> {
    if !io::stdin().is_terminal() {
        return confirm_meeting_gui(file_time, &meetings);
    }

    let sep = "─".repeat(62);
    println!(
        "\n{sep}\nMemo:     {}\nRecorded: {}",
        memo_path.file_name().unwrap_or_default().to_string_lossy(),
        file_time.format("%Y-%m-%d %H:%M:%S"),
    );
    let preview: String = transcript.chars().take(200).collect();
    let ellipsis = if transcript.chars().count() > 200 { "…" } else { "" };
    println!("Preview:  {}{ellipsis}", preview.replace('\n', " "));

    if meetings.is_empty() {
        println!("\nNo matching calendar events found.");
    } else {
        println!("\n{} calendar event(s) found nearby:", meetings.len());
        for (i, m) in meetings.iter().enumerate() {
            let att = &m.attendees;
            let mut att_str = att.iter().take(4).cloned().collect::<Vec<_>>().join(", ");
            if att.len() > 4 {
                att_str.push_str(&format!(" +{} more", att.len() - 4));
            }
            println!("\n  [{}] {}", i + 1, m.title);
            println!("       {} – {}", m.start_time, m.end_time);
            if !att_str.is_empty() {
                println!("       {att_str}");
            }
        }
    }

    let n = meetings.len();
    println!("\n  [{}] Enter meeting details manually", n + 1);
    println!("  [0] Skip");
    println!("{sep}");

    let choice = loop {
        let raw = prompt_line(&format!("Select [0–{}]: ", n + 1));
        if let Ok(c) = raw.parse::<usize>() {
            if c <= n + 1 {
                break c;
            }
        }
    };

    if choice == 0 {
        return None;
    }
    if choice <= n {
        return Some(meetings[choice - 1].clone());
    }

    let title = prompt_line("Meeting title: ");
    let title = if title.is_empty() {
        format!("Meeting {}", file_time.format("%Y-%m-%d %H:%M"))
    } else {
        title
    };
    let att_raw = prompt_line("Attendees (comma-separated, or blank): ");
    let attendees = att_raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let default_start = file_time.format("%H:%M").to_string();
    let start_raw = prompt_line(&format!("Start time (default {default_start}): "));
    let start_time = if start_raw.is_empty() {
        default_start
    } else {
        start_raw
    };

    Some(Meeting {
        title,
        attendees,
        start_time,
        ..Default::default()
    })
}

// ── Obsidian output ───────────────────────────────────────────────────────────

fn build_note_markdown(
    note_data: &NoteData,
    file_time: &DateTime<Local>,
    meeting: Option<&Meeting>,
    transcript: &str,
) -> Result<String> {
    let title = &note_data.title;
    let mut lines = vec!["---".to_string()];
    lines.push(format!("title: {}", serde_json::to_string(title)?));
    lines.push(format!("date: {}", file_time.format("%Y-%m-%d")));
    lines.push(format!(
        "time: {}",
        serde_json::to_string(&file_time.format("%H:%M").to_string())?
    ));
    lines.push("type: meeting-note".to_string());

    if let Some(m) = meeting {
        lines.push(format!("meeting: {}", serde_json::to_string(&m.title)?));
        if !m.start_time.is_empty() {
            lines.push(format!("meeting_start: {}", serde_json::to_string(&m.start_time)?));
        }
        if !m.end_time.is_empty() {
            lines.push(format!("meeting_end: {}", serde_json::to_string(&m.end_time)?));
        }
        if !m.attendees.is_empty() {
            lines.push("attendees:".to_string());
            for a in &m.attendees {
                lines.push(format!("  - {}", serde_json::to_string(a)?));
            }
        }
    }

    if !note_data.tags.is_empty() {
        lines.push("tags:".to_string());
        for tag in &note_data.tags {
            lines.push(format!("  - {tag}"));
        }
    }
    lines.push("---\n".to_string());

    let mut body = lines.join("\n");
    body.push_str(&format!("# {title}\n\n## Summary\n\n"));
    body.push_str(note_data.summary.trim());
    body.push_str("\n\n## Transcript\n\n");
    body.push_str(transcript.trim());
    body.push('\n');
    Ok(body)
}

fn write_obsidian_note(
    output_dir: &str,
    note_data: &NoteData,
    file_time: &DateTime<Local>,
    meeting: Option<&Meeting>,
    transcript: &str,
) -> Result<PathBuf> {
    let out = PathBuf::from(shellexpand::tilde(output_dir).as_ref());
    std::fs::create_dir_all(&out)?;

    let safe_title: String = note_data.title
        .chars()
        .filter(|c| !r#"<>:"/\|?*"#.contains(*c))
        .take(60)
        .collect::<String>()
        .trim()
        .to_string();
    let filepath = out.join(format!("{} {}.md", file_time.format("%Y-%m-%d"), safe_title));
    let body = build_note_markdown(note_data, file_time, meeting, transcript)?;
    std::fs::write(&filepath, body)?;
    Ok(filepath)
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn process_memo_file(memo_path: &Path, config: &Config, dry_run: bool, keep: bool) -> Result<()> {
    let prefix = if dry_run { "[dry-run] " } else { "" };
    println!("\n{prefix}Processing: {}", memo_path.file_name().unwrap_or_default().to_string_lossy());

    let file_time = get_file_creation_time(memo_path);
    println!("Recorded:  {}", file_time.format("%Y-%m-%d %H:%M:%S"));

    print!("Extracting transcript… ");
    io::stdout().flush()?;
    let transcript = get_transcript(memo_path, config);

    let transcript = match transcript {
        Some(t) => {
            println!("ok ({} chars)", t.len());
            t
        }
        None => {
            println!("not found.");
            if !io::stdin().is_terminal() {
                if !dry_run {
                    notify(&format!(
                        "Could not extract transcript from {}. Check Full Disk Access.",
                        memo_path.file_name().unwrap_or_default().to_string_lossy()
                    ));
                }
                return Ok(());
            }
            println!(
                "\nCould not extract transcript automatically.\n\
                Tip: grant Full Disk Access to Terminal in\n\
                     System Settings → Privacy & Security → Full Disk Access\n"
            );
            let manual = prompt_line("Paste transcript manually (or Enter to skip): ");
            if manual.is_empty() {
                return Ok(());
            }
            manual
        }
    };

    print!("Correcting transcript with Claude… ");
    io::stdout().flush()?;
    let corrected = correct_transcript(&transcript, config)?;
    println!("done");

    print!("Querying Google Calendar… ");
    io::stdout().flush()?;
    let meetings = find_meetings(&file_time, config);
    println!("{} event(s) found", meetings.len());

    let Some(meeting) = confirm_meeting(memo_path, &file_time, meetings, &corrected) else {
        println!("Skipped.");
        return Ok(());
    };

    print!("\nGenerating summary… ");
    io::stdout().flush()?;
    let note_data = generate_note_content(&corrected, Some(&meeting), &file_time, config)?;
    println!("done");

    if dry_run {
        let note = build_note_markdown(&note_data, &file_time, Some(&meeting), &corrected)?;
        println!("\n{}\n", "─".repeat(62));
        print!("{note}");
        println!("{}\n", "─".repeat(62));
        return Ok(());
    }

    let filepath =
        write_obsidian_note(&config.output_dir, &note_data, &file_time, Some(&meeting), &corrected)?;
    println!("\nSaved: {}\n", filepath.display());

    if !keep {
        if let Err(e) = std::fs::remove_file(memo_path) {
            eprintln!("Warning: could not remove memo file: {e}");
        }
    }

    if !io::stdin().is_terminal() {
        notify(&format!(
            "Note saved: {}",
            filepath.file_name().unwrap_or_default().to_string_lossy()
        ));
    }

    Ok(())
}

fn watch_loop(config: &Config) -> Result<()> {
    let watch_dir = shellexpand::tilde(&config.voice_memos_dir).into_owned();
    let watch_path = PathBuf::from(&watch_dir);

    if !watch_path.exists() {
        eprintln!(
            "Error: directory not accessible: {}\n\n\
            Terminal needs Full Disk Access to read Voice Memos:\n  \
            System Settings → Privacy & Security → Full Disk Access → add Terminal",
            watch_path.display()
        );
        std::process::exit(1);
    }

    println!("Watching: {}", watch_path.display());
    println!("Press Ctrl+C to stop.\n");

    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        tx.send(res).ok();
    })?;
    watcher.watch(&watch_path, RecursiveMode::NonRecursive)?;

    let mut seen: HashSet<PathBuf> = HashSet::new();

    for res in &rx {
        let event = match res {
            Ok(e) => e,
            Err(e) => {
                eprintln!("Watch error: {e}");
                continue;
            }
        };

        let is_new = matches!(
            event.kind,
            EventKind::Create(_)
                | EventKind::Modify(notify::event::ModifyKind::Name(
                    notify::event::RenameMode::To
                ))
        );
        if !is_new {
            continue;
        }

        for path in event.paths {
            if path.extension().and_then(|e| e.to_str()) != Some("m4a") {
                continue;
            }
            if !seen.insert(path.clone()) {
                continue;
            }
            println!(
                "[{}] New memo: {}",
                Local::now().format("%H:%M:%S"),
                path.file_name().unwrap_or_default().to_string_lossy()
            );
            // Small delay — let the file finish writing and Apple start transcribing
            thread::sleep(Duration::from_secs(3));
            if let Err(e) = process_memo_file(&path, config, false, false) {
                eprintln!("Error processing {}: {e}", path.display());
            }
        }
    }

    Ok(())
}

fn backfill(config: &Config, dry_run: bool, keep: bool) -> Result<()> {
    let dir = PathBuf::from(shellexpand::tilde(&config.voice_memos_dir).as_ref());

    if !dir.exists() {
        eprintln!(
            "Error: directory not accessible: {}\n\n\
            Binary needs Full Disk Access to read Voice Memos:\n  \
            System Settings → Privacy & Security → Full Disk Access",
            dir.display()
        );
        std::process::exit(1);
    }

    let mut memos: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("m4a"))
        .collect();

    memos.sort();

    if memos.is_empty() {
        println!("No .m4a files found in {}", dir.display());
        return Ok(());
    }

    println!("Found {} memo(s) to process.\n", memos.len());

    let mut errors = 0usize;
    for memo in &memos {
        if let Err(e) = process_memo_file(memo, config, dry_run, keep) {
            eprintln!("Error processing {}: {e}", memo.display());
            errors += 1;
        }
    }

    println!(
        "\nBackfill complete: {}/{} processed successfully.",
        memos.len() - errors,
        memos.len()
    );

    Ok(())
}

fn main() -> Result<()> {
    let raw_args: Vec<String> = std::env::args().collect();
    let dry_run = raw_args.contains(&"--dry-run".to_string());
    let keep = raw_args.contains(&"--keep".to_string());
    let args: Vec<&str> = raw_args[1..]
        .iter()
        .filter(|a| *a != "--dry-run" && *a != "--keep")
        .map(String::as_str)
        .collect();

    let config = load_config()?;

    match args.first().copied() {
        Some("watch") => watch_loop(&config)?,
        Some("backfill") => backfill(&config, dry_run, keep)?,
        Some("--print-watch-dir") => {
            println!("{}", shellexpand::tilde(&config.voice_memos_dir));
        }
        Some(path) => {
            let memo_path = Path::new(path);
            if !memo_path.exists() {
                eprintln!("File not found: {}", memo_path.display());
                std::process::exit(1);
            }
            process_memo_file(memo_path, &config, dry_run, keep)?;
        }
        None => {
            eprintln!("Usage: process_memo [--dry-run] [--keep] <path-to-memo.m4a>");
            eprintln!("       process_memo [--dry-run] [--keep] backfill");
            eprintln!("       process_memo watch");
            std::process::exit(1);
        }
    }

    Ok(())
}
