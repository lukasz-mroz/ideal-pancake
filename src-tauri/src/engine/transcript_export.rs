//! Writes finished transcripts to disk as Markdown files.
//!
//! Meetings captured automatically are of no use locked inside the database if
//! another tool is supposed to read them, so every automatic recording also
//! lands in `<app data>/transcripts` - `data/transcripts` next to the
//! executable in a portable build - as a single self-describing file.

use std::path::PathBuf;

use chrono::{DateTime, Local};
use log::info;

use crate::configuration::portable;

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

    let body = format!(
        "# {source_app} - {date}\n\n\
         - Source: {source_app}\n\
         - Started: {started}\n\
         - Ended: {ended}\n\
         - Duration: {minutes}m {seconds}s\n\
         - Words: {words}\n\
         - Captured automatically by Platypus (local Whisper)\n\n\
         ---\n\n\
         {text}\n",
        source_app = source_app,
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
