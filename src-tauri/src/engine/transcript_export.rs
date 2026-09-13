//! Writes finished transcripts to disk as Markdown files.
//!
//! Meetings captured automatically are of no use locked inside the database if
//! another tool is supposed to read them, so every automatic recording also
//! lands in `<app data>/transcripts` - `data/transcripts` next to the
//! executable in a portable build - as a single self-describing file.

use std::path::PathBuf;

use chrono::{DateTime, Local};
use log::info;
use serde::Serialize;

use crate::configuration::portable;

/// Version of the on-disk format, so a reader can tell what it is looking at
/// when these files outlive the build that wrote them.
pub const SCHEMA_VERSION: u32 = 1;

/// One stretch of speech, timed against the start of the recording.
#[derive(Clone, Debug, Serialize)]
pub struct Utterance {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker: String,
    pub text: String,
}

/// Stable identifier for a meeting, carried by all of its files.
///
/// Filenames are not identity: two meetings can start in the same minute, a
/// file can be renamed, and the index would then point at nothing. This is
/// derived from the start time and the source, and never changes.
pub fn meeting_id(source_app: &str, started_at: DateTime<Local>) -> String {
    let mut hash: u32 = 2166136261;
    for byte in source_app.as_bytes() {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(16777619);
    }
    format!("{}-{:04x}", started_at.format("%Y%m%dT%H%M%S"), hash & 0xffff)
}

/// Directory transcripts are written to.
pub fn transcripts_dir(app_handle: &tauri::AppHandle) -> Option<PathBuf> {
    portable::app_data_dir(app_handle).map(|dir| dir.join("transcripts"))
}

fn slugify(value: &str) -> String {
    let mut slug = String::with_capacity(value.len());
    let mut last_dash = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

/// Write one transcript. Returns the path written.
pub fn write_markdown(
    app_handle: &tauri::AppHandle,
    source_app: &str,
    other_party: Option<&str>,
    text: &str,
    started_at: DateTime<Local>,
    ended_at: DateTime<Local>,
) -> Result<PathBuf, String> {
    let dir = transcripts_dir(app_handle).ok_or_else(|| "Could not resolve app data dir".to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create {}: {}", dir.display(), e))?;

    let slug = slugify(source_app);
    let stem = format!("{}_{}", started_at.format("%Y-%m-%d_%H-%M"), slug);

    // Never overwrite an existing transcript: two meetings can start in the
    // same minute, and losing one of them silently would be worse than a
    // slightly uglier filename.
    let mut path = dir.join(format!("{}.md", stem));
    let mut suffix = 2;
    while path.exists() {
        path = dir.join(format!("{}-{}.md", stem, suffix));
        suffix += 1;
    }

    let duration = ended_at.signed_duration_since(started_at);
    let minutes = duration.num_minutes().max(0);
    let seconds = (duration.num_seconds().max(0)) % 60;
    let words = text.split_whitespace().count();

    let other_party_line = match other_party {
        Some(name) => format!("- Other party: {} (read from the window title)\n", name),
        None => String::new(),
    };

    let body = format!(
        "# {source_app} - {date}\n\n\
         - Meeting id: {meeting_id}\n\
         - Source: {source_app}\n\
         {other_party_line}\
         - Started: {started}\n\
         - Ended: {ended}\n\
         - Duration: {minutes}m {seconds}s\n\
         - Words: {words}\n\
         - Captured automatically by Platypus (local Whisper)\n\n\
         ---\n\n\
         {text}\n",
        source_app = source_app,
        meeting_id = meeting_id(source_app, started_at),
        other_party_line = other_party_line,
        date = started_at.format("%Y-%m-%d %H:%M"),
        started = started_at.format("%Y-%m-%d %H:%M:%S"),
        ended = ended_at.format("%Y-%m-%d %H:%M:%S"),
        minutes = minutes,
        seconds = seconds,
        words = words,
        text = text.trim(),
    );

    std::fs::write(&path, body).map_err(|e| format!("Failed to write {}: {}", path.display(), e))?;
    info!("Transcript written to {}", path.display());
    Ok(path)
}

/// Path of the in-progress transcript for a recording started at this time.
fn partial_path(app_handle: &tauri::AppHandle, started_at: DateTime<Local>) -> Option<PathBuf> {
    transcripts_dir(app_handle)
        .map(|dir| dir.join(format!("{}.partial.md", started_at.format("%Y-%m-%d_%H-%M-%S"))))
}

/// Write what has been transcribed so far.
///
/// The transcript otherwise only exists in memory until the meeting ends, so a
/// crash, a power cut or a killed process takes the whole meeting with it.
/// This is rewritten every few seconds; the text is small enough that the cost
/// does not matter.
pub fn write_partial(
    app_handle: &tauri::AppHandle,
    source_app: &str,
    text: &str,
    started_at: DateTime<Local>,
) {
    let Some(path) = partial_path(app_handle, started_at) else {
        return;
    };
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }

    let body = format!(
        "# {} - {} (in progress)\n\n\
         This meeting is still being recorded. The finished transcript replaces\n\
         this file when it ends.\n\n---\n\n{}\n",
        source_app,
        started_at.format("%Y-%m-%d %H:%M"),
        text.trim()
    );

    let _ = std::fs::write(path, body);
}

/// Remove the in-progress file once the final transcript has been written.
pub fn discard_partial(app_handle: &tauri::AppHandle, started_at: DateTime<Local>) {
    if let Some(path) = partial_path(app_handle, started_at) {
        let _ = std::fs::remove_file(path);
    }
}

/// Write the machine-readable companion to a transcript.
///
/// Prose is what a person reads; a model given the same meeting does better
/// with utterances it can address individually - "what was decided in the last
/// ten minutes" is a filter over timestamps, not a reading comprehension task.
pub fn write_sidecar(
    markdown_path: &PathBuf,
    source_app: &str,
    other_party: Option<&str>,
    utterances: &[Utterance],
    started_at: DateTime<Local>,
    ended_at: DateTime<Local>,
) -> Result<PathBuf, String> {
    let path = markdown_path.with_extension("json");

    // Each utterance also carries the wall-clock time it began, so joining a
    // transcript with anything else that happened that day needs no
    // arithmetic against the header.
    let timed: Vec<serde_json::Value> = utterances
        .iter()
        .map(|utterance| {
            let absolute = started_at + chrono::Duration::milliseconds(utterance.start_ms as i64);
            serde_json::json!({
                "start_ms": utterance.start_ms,
                "end_ms": utterance.end_ms,
                "start_iso": absolute.to_rfc3339(),
                "speaker": utterance.speaker,
                "text": utterance.text,
            })
        })
        .collect();

    let payload = serde_json::json!({
        "schema": SCHEMA_VERSION,
        "meeting_id": meeting_id(source_app, started_at),
        "source_app": source_app,
        "other_party": other_party,
        "started_at": started_at.to_rfc3339(),
        "ended_at": ended_at.to_rfc3339(),
        "duration_seconds": ended_at.signed_duration_since(started_at).num_seconds().max(0),
        "transcript_file": markdown_path.file_name().map(|name| name.to_string_lossy().to_string()),
        "utterances": timed,
    });

    let body = serde_json::to_string_pretty(&payload)
        .map_err(|e| format!("Could not serialise transcript: {}", e))?;
    std::fs::write(&path, body).map_err(|e| format!("Failed to write {}: {}", path.display(), e))?;
    Ok(path)
}

/// Append one line to the meeting index.
///
/// A folder of transcripts becomes unsearchable once there are a few hundred
/// of them. This is a JSON Lines file - one meeting per line, appended, never
/// rewritten - so anything reading it can find the right meeting without
/// opening every file.
pub fn append_index(
    app_handle: &tauri::AppHandle,
    markdown_path: &PathBuf,
    source_app: &str,
    other_party: Option<&str>,
    text: &str,
    started_at: DateTime<Local>,
    ended_at: DateTime<Local>,
) -> Result<(), String> {
    use std::io::Write;

    let dir = transcripts_dir(app_handle).ok_or_else(|| "Could not resolve app data dir".to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create {}: {}", dir.display(), e))?;
    let index = dir.join("index.jsonl");

    let entry = serde_json::json!({
        "schema": SCHEMA_VERSION,
        "meeting_id": meeting_id(source_app, started_at),
        "event": "meeting",
        "started_at": started_at.to_rfc3339(),
        "ended_at": ended_at.to_rfc3339(),
        "duration_seconds": ended_at.signed_duration_since(started_at).num_seconds().max(0),
        "source_app": source_app,
        "other_party": other_party,
        "words": text.split_whitespace().count(),
        "transcript": markdown_path.file_name().map(|name| name.to_string_lossy().to_string()),
    });

    let line = serde_json::to_string(&entry)
        .map_err(|e| format!("Could not serialise index entry: {}", e))?;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&index)
        .map_err(|e| format!("Failed to open {}: {}", index.display(), e))?;
    writeln!(file, "{}", line).map_err(|e| format!("Failed to write {}: {}", index.display(), e))?;
    Ok(())
}

/// Fold the extraction result into an existing sidecar.
///
/// Written as a separate step on purpose: the transcript and its timings are
/// saved before any model is asked anything, so a model that is unreachable,
/// slow or wrong can never cost the recording itself.
pub fn attach_analysis(
    sidecar: &PathBuf,
    raw_model_output: &str,
) -> Result<serde_json::Value, String> {
    let existing = std::fs::read_to_string(sidecar)
        .map_err(|e| format!("Failed to read {}: {}", sidecar.display(), e))?;
    let mut payload: serde_json::Value = serde_json::from_str(&existing)
        .map_err(|e| format!("Sidecar is not valid JSON: {}", e))?;

    // Small models like to wrap JSON in prose or code fences; keep what parses
    // and fall back to storing the text so nothing is silently lost.
    let trimmed = raw_model_output.trim();
    let cleaned = trimmed
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let analysis = match serde_json::from_str::<serde_json::Value>(cleaned) {
        Ok(parsed) => parsed,
        Err(_) => match (cleaned.find('{'), cleaned.rfind('}')) {
            (Some(start), Some(end)) if end > start => {
                serde_json::from_str::<serde_json::Value>(&cleaned[start..=end])
                    .unwrap_or_else(|_| serde_json::json!({ "raw": cleaned }))
            }
            _ => serde_json::json!({ "raw": cleaned }),
        },
    };

    payload["analysis"] = analysis.clone();

    let body = serde_json::to_string_pretty(&payload)
        .map_err(|e| format!("Could not serialise sidecar: {}", e))?;
    std::fs::write(sidecar, body)
        .map_err(|e| format!("Failed to write {}: {}", sidecar.display(), e))?;
    Ok(analysis)
}

/// Append the extraction result to the index as a second line for the same
/// meeting, so a reader can find what a meeting was about without opening its
/// sidecar.
///
/// The index stays append-only: nothing is rewritten, and a later line simply
/// carries more about a meeting already recorded.
pub fn append_index_analysis(
    app_handle: &tauri::AppHandle,
    meeting_id: &str,
    analysis: &serde_json::Value,
) -> Result<(), String> {
    use std::io::Write;

    let dir =
        transcripts_dir(app_handle).ok_or_else(|| "Could not resolve app data dir".to_string())?;
    let index = dir.join("index.jsonl");

    let summary = analysis.get("summary").and_then(|value| value.as_str());
    let decisions = analysis
        .get("decisions")
        .and_then(|value| value.as_array())
        .map(|items| items.len())
        .unwrap_or(0);

    let entry = serde_json::json!({
        "schema": SCHEMA_VERSION,
        "meeting_id": meeting_id,
        "event": "analysis",
        "summary": summary,
        "decisions": decisions,
    });

    let line = serde_json::to_string(&entry)
        .map_err(|e| format!("Could not serialise index entry: {}", e))?;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&index)
        .map_err(|e| format!("Failed to open {}: {}", index.display(), e))?;
    writeln!(file, "{}", line).map_err(|e| format!("Failed to write {}: {}", index.display(), e))?;
    Ok(())
}

/// Finish transcripts left behind by a recording that never stopped cleanly.
///
/// A `.partial.md` file means the process died mid-meeting. Rather than leave
/// it as a half-file nobody reads, promote it to a normal transcript marked as
/// interrupted and record it in the index, so the meeting is still there.
pub fn recover_interrupted(app_handle: &tauri::AppHandle) {
    let Some(dir) = transcripts_dir(app_handle) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().map(|n| n.to_string_lossy().to_string());
        let Some(name) = name else { continue };
        if !name.ends_with(".partial.md") {
            continue;
        }

        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        let stem = name.trim_end_matches(".partial.md").to_string();
        let recovered = dir.join(format!("{}-interrupted.md", stem));

        let annotated = body.replace(
            "This meeting is still being recorded. The finished transcript replaces\nthis file when it ends.",
            "This recording was interrupted - the application stopped before the\nmeeting ended. What follows is everything that had been transcribed.",
        );

        if std::fs::write(&recovered, annotated).is_ok() {
            let _ = std::fs::remove_file(&path);
            info!("Recovered an interrupted transcript as {}", recovered.display());
        }
    }
}
