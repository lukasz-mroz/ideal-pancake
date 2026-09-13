//! Portable mode.
//!
//! In portable mode every piece of user data — the SQLite database, HNSW
//! indices, Whisper models, podcasts and recordings — lives in a `data`
//! folder next to the executable instead of the operating system's app-data
//! directory. The whole app can then be carried on a USB stick and leaves no
//! traces on the host machine.
//!
//! Portable mode is on when:
//!   * the binary was compiled with the `portable` cargo feature, or
//!   * a file named `portable.txt` sits next to the executable, or
//!   * the environment variable `PLATYPUS_PORTABLE` is `1`/`true`/`yes`/`on`.
//!
//! It is forced off with `PLATYPUS_PORTABLE=0`. If the folder next to the
//! executable is not writable (an install under `C:\Program Files`, a
//! read-only medium), we silently fall back to the regular OS paths so the
//! app still starts.

use std::path::{Path, PathBuf};

use once_cell::sync::OnceCell;

static PORTABLE_ROOT: OnceCell<Option<PathBuf>> = OnceCell::new();

fn env_override() -> Option<bool> {
    let raw = std::env::var("PLATYPUS_PORTABLE").ok()?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(|parent| parent.to_path_buf())
}

fn is_writable(dir: &Path) -> bool {
    let probe = dir.join(".platypus_write_test");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn detect() -> Option<PathBuf> {
    let exe_dir = exe_dir()?;

    let enabled = match env_override() {
        Some(value) => value,
        None => cfg!(feature = "portable") || exe_dir.join("portable.txt").exists(),
    };
    if !enabled {
        return None;
    }

    let root = exe_dir.join("data");
    if std::fs::create_dir_all(&root).is_err() {
        return None;
    }
    if !is_writable(&root) {
        return None;
    }
    Some(root)
}

/// `<exe dir>/data` when running portable, `None` otherwise.
pub fn portable_root() -> Option<&'static PathBuf> {
    PORTABLE_ROOT.get_or_init(detect).as_ref()
}

pub fn is_portable() -> bool {
    portable_root().is_some()
}

/// Directory holding the database, indices and generated files.
///
/// Portable: `<exe dir>/data`. Otherwise the usual Tauri app-data directory.
pub fn app_data_dir(app_handle: &tauri::AppHandle) -> Option<PathBuf> {
    use tauri::Manager;

    if let Some(root) = portable_root() {
        return Some(root.clone());
    }
    app_handle.path_resolver().app_data_dir()
}

/// Same as [`app_data_dir`], panicking with the original message when the OS
/// path cannot be resolved (kept for call sites that always expected a path).
pub fn app_data_dir_or_panic(app_handle: &tauri::AppHandle) -> PathBuf {
    app_data_dir(app_handle).expect("The app data directory should exist.")
}

/// Where Whisper models are stored.
pub fn models_dir() -> PathBuf {
    match portable_root() {
        Some(root) => root.join("models"),
        None => dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("platypus_notes")
            .join("models"),
    }
}

/// Where in-progress recordings are written.
pub fn recordings_dir() -> PathBuf {
    match portable_root() {
        Some(root) => root.join("recordings"),
        None => std::env::temp_dir().join("platypus_recordings"),
    }
}

/// Point WebView2 at the portable folder so cookies, localStorage and cache
/// stay inside the app folder too. Must run before the first window is
/// created.
pub fn prepare_environment() {
    if let Some(root) = portable_root() {
        if std::env::var_os("WEBVIEW2_USER_DATA_FOLDER").is_none() {
            let webview_dir = root.join("webview");
            if std::fs::create_dir_all(&webview_dir).is_ok() {
                std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", &webview_dir);
            }
        }
    }
}
