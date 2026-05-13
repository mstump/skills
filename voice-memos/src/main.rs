use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Local, TimeZone};
use clap::{Args, Parser, Subcommand};
use is_terminal::IsTerminal;
use notify::{EventKind, RecursiveMode, Watcher};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use tracing::{debug, error, info, warn};

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

fn extract_via_tsrp(path: &Path) -> Option<String> {
    let data = std::fs::read(path).ok()?;
    let payload = find_atom(&data, b"tsrp")?;
    let json: serde_json::Value = serde_json::from_slice(payload).ok()?;
    let attributed = json.get("attributedString")?;

    let text: String = if let Some(arr) = attributed.as_array() {
        // format 1: ["word", {"timeRange": [...]}, "word", ...]
        arr.iter().filter_map(|v| v.as_str()).collect()
    } else if let Some(runs) = attributed.get("runs").and_then(|r| r.as_array()) {
        // format 2: {"runs": ["word", 0, "word", 1, ...]}
        runs.iter().filter_map(|v| v.as_str()).collect()
    } else {
        return None;
    };

    let text = text.trim().to_string();
    if text.len() > 5 { Some(text) } else { None }
}

fn get_transcript(path: &Path, config: &Config) -> Option<String> {
    let attempts = 3u32;
    let delay = Duration::from_secs((config.transcript_wait_seconds / attempts as u64).max(1));
    for attempt in 0..attempts {
        let t = extract_via_tsrp(path).or_else(|| extract_via_mdls(path));
        if t.is_some() {
            return t;
        }
        if attempt < attempts - 1 {
            info!(attempt = attempt + 1, of = attempts - 1, "waiting for transcript");
            thread::sleep(delay);
        }
    }
    None
}

// ── Claude API ────────────────────────────────────────────────────────────────

fn call_claude(model: &str, prompt: &str) -> Result<String> {
    debug!(model, prompt_chars = prompt.len(), "calling claude CLI");
    let mut child = Command::new("claude")
        .args(["-p", prompt, "--model", model])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("claude CLI not found — install Claude Code from https://claude.ai/code")?;

    // Drain stdout/stderr on separate threads to avoid pipe-buffer deadlock when
    // the subprocess writes more than the OS buffer (typically 64 KB) before exiting.
    let mut child_stdout = child.stdout.take().unwrap();
    let mut child_stderr = child.stderr.take().unwrap();
    let stdout_thread = thread::spawn(move || {
        let mut buf = String::new();
        child_stdout.read_to_string(&mut buf).ok();
        buf
    });
    let stderr_thread = thread::spawn(move || {
        let mut buf = String::new();
        child_stderr.read_to_string(&mut buf).ok();
        buf
    });

    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(anyhow!("claude CLI timed out after 180s"));
            }
            Ok(None) => thread::sleep(Duration::from_millis(200)),
            Err(e) => return Err(e.into()),
        }
    }

    let status = child.wait()?;
    let stdout_str = stdout_thread.join().unwrap_or_default();
    let stderr_str = stderr_thread.join().unwrap_or_default();

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        let detail = if !stderr_str.trim().is_empty() {
            stderr_str.trim().to_string()
        } else if !stdout_str.trim().is_empty() {
            stdout_str.trim().to_string()
        } else {
            format!("exit code {code}, no output")
        };
        debug!(%detail, "claude CLI stderr: {}", stderr_str.trim());
        return Err(anyhow!("claude CLI failed (exit {code}): {detail}"));
    }

    let result = stdout_str.trim().to_string();
    debug!(response_chars = result.len(), "claude CLI responded");
    Ok(result)
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

    let window = config.time_window_minutes;
    let start = (*file_time - chrono::Duration::minutes(window))
        .format("%Y-%m-%dT%H:%M:%S");
    let end = (*file_time + chrono::Duration::minutes(window))
        .format("%Y-%m-%dT%H:%M:%S");

    let prompt = format!(
        "Using Google Calendar, list events between {start} and {end} in local time. \
        Return a JSON array only — no prose. Each element: \
        {{\"title\": \"...\", \"start_time\": \"ISO8601\", \"end_time\": \"ISO8601\", \
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
            warn!("claude CLI not found, skipping calendar lookup");
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

// ── Meeting scoring ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScoredMeeting {
    #[serde(flatten)]
    meeting: Meeting,
    duration_minutes: Option<i64>,
    relevance_score: u8,
    relevance_reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct PreparedMemo {
    memo_path: String,
    recorded: String,
    transcript: String,
    meetings: Vec<ScoredMeeting>,
    recommended_index: Option<usize>,
}

impl PreparedMemo {
    fn recorded_dt(&self) -> Result<DateTime<Local>> {
        chrono::DateTime::parse_from_rfc3339(&self.recorded)
            .map(|dt| dt.with_timezone(&Local))
            .context("parsing recorded timestamp")
    }
}

fn compute_duration_minutes(start: &str, end: &str) -> Option<i64> {
    // Try ISO 8601 full datetime first
    if let (Ok(s), Ok(e)) = (
        chrono::DateTime::parse_from_rfc3339(start),
        chrono::DateTime::parse_from_rfc3339(end),
    ) {
        let diff = (e - s).num_minutes();
        return Some(diff.abs());
    }

    // Try HH:MM or HH:MM:SS
    let parse_hhmm = |s: &str| -> Option<i64> {
        let s = s.trim();
        let mut parts = s.splitn(3, ':');
        let h: i64 = parts.next()?.trim().parse().ok()?;
        let m: i64 = parts.next()?.trim().parse().ok()?;
        Some(h * 60 + m)
    };

    let sm = parse_hhmm(start)?;
    let em = parse_hhmm(end)?;
    let diff = em - sm;
    Some(if diff >= 0 { diff } else { diff + 24 * 60 })
}

fn score_meetings(
    transcript: &str,
    meetings: &[Meeting],
    file_time: &DateTime<Local>,
    config: &Config,
) -> Vec<ScoredMeeting> {
    if meetings.is_empty() {
        return vec![];
    }

    let meeting_list = meetings
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let dur = compute_duration_minutes(&m.start_time, &m.end_time);
            let dur_str = dur.map(|d| format!(" ({d} min)")).unwrap_or_default();
            let att = if m.attendees.is_empty() {
                "(none)".to_string()
            } else {
                m.attendees.join(", ")
            };
            let desc = m.description.as_deref().unwrap_or("(none)");
            format!(
                "[{i}] {}{} | {} – {}{dur_str}\n    Attendees: {att}\n    Description: {desc}",
                m.title,
                if m.title.is_empty() { "(no title)" } else { "" },
                m.start_time,
                m.end_time,
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let preview: String = transcript.chars().take(600).collect();
    let ellipsis = if transcript.chars().count() > 600 { "…" } else { "" };

    let prompt = format!(
        "Match this voice memo to its most likely Google Calendar event.\n\
        Recording time: {}\n\n\
        Transcript excerpt:\n{preview}{ellipsis}\n\n\
        Nearby calendar events:\n{meeting_list}\n\n\
        Score each event 0–100 for how likely this transcript was recorded during it.\n\
        Consider: timing overlap, names/topics in the transcript vs event title/description/attendees.\n\
        Return ONLY valid JSON array: \
        [{{\"index\":0,\"score\":85,\"reason\":\"one sentence\"}},...]",
        file_time.format("%Y-%m-%d %H:%M")
    );

    let scores: Vec<serde_json::Value> = match call_claude(&config.model, &prompt) {
        Ok(raw) => {
            let re = Regex::new(r"(?s)\[.*\]").unwrap();
            re.find(&raw)
                .and_then(|m| serde_json::from_str(m.as_str()).ok())
                .unwrap_or_default()
        }
        Err(e) => {
            warn!("meeting scoring failed: {e}");
            vec![]
        }
    };

    meetings
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let entry = scores
                .iter()
                .find(|s| s.get("index").and_then(|v| v.as_u64()) == Some(i as u64));
            let (relevance_score, relevance_reason) = entry
                .map(|s| {
                    (
                        s.get("score").and_then(|v| v.as_u64()).unwrap_or(0) as u8,
                        s.get("reason")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    )
                })
                .unwrap_or((0, String::new()));

            ScoredMeeting {
                duration_minutes: compute_duration_minutes(&m.start_time, &m.end_time),
                meeting: m.clone(),
                relevance_score,
                relevance_reason,
            }
        })
        .collect()
}

fn prepare_memo(path: &Path, config: &Config) -> Result<PreparedMemo> {
    info!(path = %path.display(), "preparing memo");

    let file_time = get_file_creation_time(path);
    info!(recorded = %file_time.format("%Y-%m-%d %H:%M:%S"), "file timestamp");

    info!("extracting transcript");
    let raw = get_transcript(path, config);
    let raw = match raw {
        Some(t) => {
            info!(chars = t.len(), "transcript extracted");
            if t.len() < 50 {
                debug!(content = %t, "short transcript content");
            }
            t
        }
        None => {
            return Err(anyhow!("no transcript found"));
        }
    };

    info!("correcting transcript");
    let transcript = correct_transcript(&raw, config)?;
    info!(chars = transcript.len(), "transcript corrected");

    info!("finding meetings (±{} min)", config.time_window_minutes);
    let meetings = find_meetings(&file_time, config);
    info!(count = meetings.len(), "meetings found");

    let scored = if meetings.is_empty() {
        vec![]
    } else {
        info!("scoring meeting relevance");
        let s = score_meetings(&transcript, &meetings, &file_time, config);
        info!("scoring complete");
        s
    };

    let recommended_index = scored
        .iter()
        .enumerate()
        .max_by_key(|(_, m)| m.relevance_score)
        .map(|(i, _)| i);

    Ok(PreparedMemo {
        memo_path: path.to_string_lossy().to_string(),
        recorded: file_time.to_rfc3339(),
        transcript,
        meetings: scored,
        recommended_index,
    })
}

fn finalize_memo(
    prepared: &PreparedMemo,
    select: Option<usize>,
    keep: bool,
    config: &Config,
) -> Result<PathBuf> {
    let file_time = prepared.recorded_dt()?;
    let meeting = select
        .and_then(|i| prepared.meetings.get(i))
        .map(|sm| &sm.meeting);

    info!("generating note");
    let note_data = generate_note_content(&prepared.transcript, meeting, &file_time, config)?;

    let filepath = write_obsidian_note(
        &config.output_dir,
        &note_data,
        &file_time,
        meeting,
        &prepared.transcript,
    )?;
    info!(path = %filepath.display(), "note saved");

    if !keep {
        if let Err(e) = std::fs::remove_file(&prepared.memo_path) {
            warn!("could not remove memo file: {e}");
        }
    }

    Ok(filepath)
}

// ── macOS notifications ───────────────────────────────────────────────────────

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

// ── Pipeline ──────────────────────────────────────────────────────────────────

/// Full auto-select pipeline for a single file (watch mode, backfill --auto, run).
/// Selects the highest-scoring meeting above the 50-point threshold; saves without
/// meeting metadata if nothing clears the bar.
fn process_memo_file(memo_path: &Path, config: &Config, dry_run: bool, keep: bool) -> Result<()> {
    let prepared = prepare_memo(memo_path, config)?;

    let select = prepared.recommended_index.filter(|&i| {
        prepared
            .meetings
            .get(i)
            .map(|m| m.relevance_score >= 50)
            .unwrap_or(false)
    });

    if let Some(i) = select {
        if let Some(m) = prepared.meetings.get(i) {
            info!(
                meeting = %m.meeting.title,
                score = m.relevance_score,
                "auto-selected meeting"
            );
        }
    } else {
        info!("no meeting matched threshold, saving without meeting metadata");
    }

    if dry_run {
        let file_time = prepared.recorded_dt()?;
        let meeting = select.and_then(|i| prepared.meetings.get(i)).map(|sm| &sm.meeting);
        let note_data = generate_note_content(&prepared.transcript, meeting, &file_time, config)?;
        let note = build_note_markdown(&note_data, &file_time, meeting, &prepared.transcript)?;
        println!("{note}");
        return Ok(());
    }

    let filepath = finalize_memo(&prepared, select, keep, config)?;

    if !io::stdin().is_terminal() {
        notify(&format!(
            "Note saved: {}",
            filepath.file_name().unwrap_or_default().to_string_lossy()
        ));
    }

    Ok(())
}

fn watch_loop(config: &Config, watch_path: &Path) -> Result<()> {
    info!(path = %watch_path.display(), "watching for new memos, press ctrl-c to stop");

    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        tx.send(res).ok();
    })?;
    watcher.watch(watch_path, RecursiveMode::NonRecursive)?;

    let mut seen: HashSet<PathBuf> = HashSet::new();

    for res in &rx {
        let event = match res {
            Ok(e) => e,
            Err(e) => {
                error!("watch error: {e}");
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
            info!(path = %path.display(), "new memo detected");
            // Small delay — let the file finish writing and Apple start transcribing
            thread::sleep(Duration::from_secs(3));
            if let Err(e) = process_memo_file(&path, config, false, false) {
                error!(path = %path.display(), error = %e, "processing failed");
                if !io::stdin().is_terminal() {
                    notify(&format!(
                        "Failed: {}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ));
                }
            }
        }
    }

    Ok(())
}

fn check_voice_memos_access(config: &Config) -> PathBuf {
    let dir = PathBuf::from(shellexpand::tilde(&config.voice_memos_dir).as_ref());
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            let count = entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("m4a"))
                .count();
            info!(path = %dir.display(), memos = count, "voice memos accessible");
        }
        Err(e) => {
            error!(
                path = %dir.display(),
                error = %e,
                "cannot access voice memos — grant Full Disk Access in System Settings → Privacy & Security → Full Disk Access"
            );
            std::process::exit(1);
        }
    }
    dir
}

/// Prepare-only backfill: runs prepare_memo for each file and writes one JSON
/// object per line to stdout (NDJSON). Used by the interactive skill command.
fn backfill_prepare(config: &Config, dir: &Path) -> Result<()> {
    let mut memos: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("m4a"))
        .collect();
    memos.sort();

    if memos.is_empty() {
        info!("no memos found");
        return Ok(());
    }

    info!(count = memos.len(), "preparing memos");
    for memo in &memos {
        match prepare_memo(memo, config) {
            Ok(prepared) => {
                println!("{}", serde_json::to_string(&prepared)?);
            }
            Err(e) => {
                error!(path = %memo.display(), error = %e, "prepare failed");
            }
        }
    }
    Ok(())
}

/// Auto-select backfill: full pipeline with no user interaction.
fn backfill_auto(config: &Config, dir: &Path, dry_run: bool, keep: bool) -> Result<()> {
    let mut memos: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("m4a"))
        .collect();
    memos.sort();

    if memos.is_empty() {
        info!("no memos to process");
        return Ok(());
    }

    info!(count = memos.len(), "starting backfill");

    let mut errors = 0usize;
    for memo in &memos {
        if let Err(e) = process_memo_file(memo, config, dry_run, keep) {
            error!(path = %memo.display(), error = %e, "processing failed");
            errors += 1;
        }
    }

    info!(
        processed = memos.len() - errors,
        total = memos.len(),
        "backfill complete"
    );

    Ok(())
}

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Args)]
struct ProcessArgs {
    /// Print the note to stdout instead of saving, and skip file deletion
    #[arg(long)]
    dry_run: bool,
    /// Process without deleting the source memo file afterward
    #[arg(long)]
    keep: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Watch for new Voice Memos and auto-process them as they arrive
    Watch,

    /// Process all existing memos; without --auto outputs NDJSON for the skill to consume
    Backfill {
        /// Automatically select best meeting match and write notes (daemon/batch mode)
        #[arg(long)]
        auto: bool,
        #[command(flatten)]
        args: ProcessArgs,
    },

    /// Process a single memo file with auto-selected meeting
    Run {
        path: PathBuf,
        #[command(flatten)]
        args: ProcessArgs,
    },

    /// Extract transcript, correct it, and score nearby meetings; outputs JSON
    Prepare {
        path: PathBuf,
    },

    /// Write an Obsidian note from a prepared JSON file with a chosen meeting
    Finalize {
        /// Path to the JSON file produced by `prepare`
        prepared_json: PathBuf,
        /// Index of meeting to use (0-based from the meetings array); omit for no meeting
        #[arg(long)]
        select: Option<usize>,
        /// Keep the source memo file (do not delete)
        #[arg(long)]
        keep: bool,
    },

    #[command(hide = true)]
    PrintWatchDir,
}

#[derive(Parser)]
#[command(name = "process-memo", about = "Voice Memos → Obsidian pipeline")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let config = load_config()?;

    match cli.cmd {
        Cmd::PrintWatchDir => {
            println!("{}", shellexpand::tilde(&config.voice_memos_dir));
        }

        Cmd::Watch => {
            let dir = check_voice_memos_access(&config);
            watch_loop(&config, &dir)?;
        }

        Cmd::Backfill { auto, args } => {
            let dir = check_voice_memos_access(&config);
            if auto {
                backfill_auto(&config, &dir, args.dry_run, args.keep)?;
            } else {
                backfill_prepare(&config, &dir)?;
            }
        }

        Cmd::Run { path, args } => {
            if !path.exists() {
                error!(path = %path.display(), "file not found");
                std::process::exit(1);
            }
            process_memo_file(&path, &config, args.dry_run, args.keep)?;
        }

        Cmd::Prepare { path } => {
            if !path.exists() {
                error!(path = %path.display(), "file not found");
                std::process::exit(1);
            }
            let prepared = prepare_memo(&path, &config)?;
            println!("{}", serde_json::to_string(&prepared)?);
        }

        Cmd::Finalize { prepared_json, select, keep } => {
            let content = std::fs::read_to_string(&prepared_json)
                .with_context(|| format!("reading {}", prepared_json.display()))?;
            let prepared: PreparedMemo = serde_json::from_str(&content)
                .context("parsing prepared JSON")?;
            let filepath = finalize_memo(&prepared, select, keep, &config)?;
            println!("{}", filepath.display());
        }
    }

    Ok(())
}
