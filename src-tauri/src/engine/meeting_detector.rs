use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use log::{debug, info};
use sysinfo::System;
use tauri::{AppHandle, Manager};

/// Runtime toggle — set from settings, read by the detection loop.
pub static MEETING_DETECTION_ENABLED: AtomicBool = AtomicBool::new(false);

/// Don't re-notify for the same app within this window.
const NOTIFICATION_COOLDOWN: Duration = Duration::from_secs(300); // 5 minutes

/// Number of consecutive "not detected" polls before we consider a meeting ended.
/// Prevents flapping when transient mic drops happen mid-call.
const LEAVE_GRACE_POLLS: u8 = 3;

/// Windows records microphone use per application in the registry: the same
/// data behind the "app is using your microphone" indicator. A key whose
/// `LastUsedTimeStop` is 0 is using the microphone *right now*.
///
/// This is per-app, which matters: the macOS path can only ask "is the input
/// device busy" globally and therefore has to suppress itself while Platypus
/// records. Here we can ask about Teams specifically and keep watching it for
/// as long as our own recording runs.
#[cfg(target_os = "windows")]
fn app_holding_mic(name_fragment: &str) -> bool {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
    use winreg::RegKey;

    const CONSENT_STORE: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";

    fn key_is_active(key: &winreg::RegKey) -> bool {
        // REG_QWORD; 0 means "still in use". Absent value means never used.
        key.get_value::<u64, _>("LastUsedTimeStop")
            .map(|stop| stop == 0)
            .unwrap_or(false)
    }

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let store = match hkcu.open_subkey_with_flags(CONSENT_STORE, KEY_READ) {
        Ok(key) => key,
        Err(err) => {
            debug!("Cannot read microphone consent store: {}", err);
            return false;
        }
    };

    let needle = name_fragment.to_ascii_lowercase();

    // Packaged apps (new Teams is one) sit directly under the store, keyed by
    // package family name. Desktop apps sit under NonPackaged, keyed by their
    // executable path with backslashes replaced by '#'.
    for name in store.enum_keys().flatten() {
        if name.eq_ignore_ascii_case("NonPackaged") {
            if let Ok(non_packaged) = store.open_subkey_with_flags(&name, KEY_READ) {
                for exe_key in non_packaged.enum_keys().flatten() {
                    if exe_key.to_ascii_lowercase().contains(&needle) {
                        if let Ok(key) = non_packaged.open_subkey_with_flags(&exe_key, KEY_READ) {
                            if key_is_active(&key) {
                                debug!("Microphone held by {}", exe_key);
                                return true;
                            }
                        }
                    }
                }
            }
            continue;
        }

        if name.to_ascii_lowercase().contains(&needle) {
            if let Ok(key) = store.open_subkey_with_flags(&name, KEY_READ) {
                if key_is_active(&key) {
                    debug!("Microphone held by {}", name);
                    return true;
                }
            }
        }
    }

    false
}

/// Check if Zoom is in an active meeting.
/// CptHost is a process that only runs during an active Zoom meeting on macOS.
/// Important: "caphost" is a DIFFERENT process that runs whenever Zoom is open
/// (even without a meeting) — do NOT match it, or you get false positives.
#[cfg(not(target_os = "windows"))]
fn is_zoom_in_meeting(system: &System) -> bool {
    for (_pid, process) in system.processes() {
        let name = process.name();
        if name == "CptHost" {
            debug!("Zoom active meeting process found: {}", name);
            return true;
        }
    }
    false
}

/// True when any running process looks like Microsoft Teams (Classic or New).
fn is_teams_running(system: &System) -> bool {
    for (_pid, process) in system.processes() {
        let name = process.name();
        // New Teams binary is "MSTeams"; Classic Teams binary is "Teams".
        if name == "MSTeams" {
            return true;
        }
        let cmd_joined = process.cmd().join(" ");
        if cmd_joined.contains("Microsoft Teams") {
            return true;
        }
    }
    false
}

/// macOS CoreAudio FFI: query whether the default input device is in use by
/// any process. This is the same property that drives the orange-dot mic
/// indicator in the menu bar — the strongest possible "someone is recording
/// right now" signal.
#[cfg(target_os = "macos")]
mod core_audio_ffi {
    use std::os::raw::c_void;

    pub type AudioObjectID = u32;
    pub type OSStatus = i32;

    #[repr(C)]
    pub struct AudioObjectPropertyAddress {
        pub m_selector: u32,
        pub m_scope: u32,
        pub m_element: u32,
    }

    pub const K_AUDIO_OBJECT_SYSTEM_OBJECT: AudioObjectID = 1;
    // FourCC literals from <CoreAudio/AudioHardware.h>.
    pub const K_DEFAULT_INPUT_DEVICE: u32 = 0x6449_6e20; // 'dIn '
    pub const K_DEVICE_IS_RUNNING_SOMEWHERE: u32 = 0x676f_6e65; // 'gone'
    pub const K_SCOPE_GLOBAL: u32 = 0x676c_6f62; // 'glob'
    pub const K_ELEMENT_MAIN: u32 = 0;

    #[link(name = "CoreAudio", kind = "framework")]
    extern "C" {
        pub fn AudioObjectGetPropertyData(
            in_object_id: AudioObjectID,
            in_address: *const AudioObjectPropertyAddress,
            in_qualifier_data_size: u32,
            in_qualifier_data: *const c_void,
            io_data_size: *mut u32,
            out_data: *mut c_void,
        ) -> OSStatus;
    }
}

/// Returns `true` when something (anything) is actively reading from the
/// default input device — i.e. the orange mic dot is currently on. We
/// subtract Platypus's own recording state so our own recorder doesn't
/// trigger false positives.
#[cfg(target_os = "macos")]
fn is_external_mic_active() -> bool {
    use core_audio_ffi::*;
    use std::ffi::c_void;

    if crate::engine::audio_engine::IS_RECORDING.load(Ordering::Relaxed) {
        // Platypus is recording — we can't distinguish "us only" from "us +
        // Teams" via this property, so suppress detection while we record.
        return false;
    }

    unsafe {
        let mut device_id: AudioObjectID = 0;
        let mut size = std::mem::size_of::<AudioObjectID>() as u32;
        let addr = AudioObjectPropertyAddress {
            m_selector: K_DEFAULT_INPUT_DEVICE,
            m_scope: K_SCOPE_GLOBAL,
            m_element: K_ELEMENT_MAIN,
        };
        let status = AudioObjectGetPropertyData(
            K_AUDIO_OBJECT_SYSTEM_OBJECT,
            &addr,
            0,
            std::ptr::null(),
            &mut size,
            &mut device_id as *mut _ as *mut c_void,
        );
        if status != 0 || device_id == 0 {
            return false;
        }

        let mut is_running: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        let addr2 = AudioObjectPropertyAddress {
            m_selector: K_DEVICE_IS_RUNNING_SOMEWHERE,
            m_scope: K_SCOPE_GLOBAL,
            m_element: K_ELEMENT_MAIN,
        };
        let status = AudioObjectGetPropertyData(
            device_id,
            &addr2,
            0,
            std::ptr::null(),
            &mut size,
            &mut is_running as *mut _ as *mut c_void,
        );
        status == 0 && is_running != 0
    }
}

#[cfg(not(target_os = "macos"))]
fn is_external_mic_active() -> bool {
    false
}

/// Optional macOS window-title scan for Teams meeting windows. Used only to
/// pick a nicer display label ("Meeting in 'Channel X'" vs the generic
/// "Microsoft Teams"); never used as a fallback signal — having a window
/// open is NOT the same as being in a meeting. Reading window titles
/// requires Screen Recording on macOS 10.15+; without it we get an empty
/// title and just return None.
#[cfg(target_os = "macos")]
fn teams_meeting_window_title() -> Option<String> {
    use core_foundation::{
        array::CFArray, base::TCFType, dictionary::CFDictionary, string::CFString,
    };
    use core_graphics::display::{
        kCGNullWindowID, kCGWindowListExcludeDesktopElements,
        kCGWindowListOptionOnScreenOnly, CGWindowListCopyWindowInfo,
    };

    unsafe {
        let opts = kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements;
        let list_ref = CGWindowListCopyWindowInfo(opts, kCGNullWindowID);
        if list_ref.is_null() {
            return None;
        }
        let arr: CFArray<CFDictionary<CFString, core_foundation::base::CFType>> =
            CFArray::wrap_under_create_rule(list_ref);

        let owner_key = CFString::new("kCGWindowOwnerName");
        let title_key = CFString::new("kCGWindowName");

        for i in 0..arr.len() {
            let Some(dict) = arr.get(i) else { continue };

            let owner = dict
                .find(&owner_key)
                .and_then(|v| v.downcast::<CFString>())
                .map(|s| s.to_string())
                .unwrap_or_default();

            let owner_is_teams = owner == "Microsoft Teams"
                || owner == "MSTeams"
                || owner.starts_with("Teams");
            if !owner_is_teams {
                continue;
            }

            let title = dict
                .find(&title_key)
                .and_then(|v| v.downcast::<CFString>())
                .map(|s| s.to_string())
                .unwrap_or_default();

            if title.contains("Meeting")
                || title.contains("Call")
                || title.contains("| Microsoft Teams")
            {
                return Some(title);
            }
        }
        None
    }
}

#[cfg(not(target_os = "macos"))]
fn teams_meeting_window_title() -> Option<String> {
    None
}

/// Teams is in a meeting iff the system mic is active AND Teams is running.
/// "Teams running" alone is not enough — that's where the previous
/// implementation got false positives. The mic-active gate matches what the
/// macOS orange-dot indicator already shows the user.
#[cfg(not(target_os = "windows"))]
fn is_teams_in_meeting(system: &System) -> bool {
    if !is_external_mic_active() {
        return false;
    }
    if !is_teams_running(system) {
        return false;
    }
    if let Some(title) = teams_meeting_window_title() {
        debug!("Teams meeting window: {}", title);
    }
    true
}

/// On Windows, "in a meeting" is simply "the app is holding the microphone".
#[cfg(target_os = "windows")]
fn is_zoom_in_meeting(_system: &System) -> bool {
    app_holding_mic("zoom")
}

/// Title of the Teams window that looks like a meeting, e.g. the meeting name
/// or the person being called. Used to name the transcript; never used to
/// decide whether a meeting is happening.
#[cfg(target_os = "windows")]
pub fn meeting_window_title(system: &System) -> Option<String> {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
    };

    struct Search {
        pids: Vec<u32>,
        found: Option<String>,
    }

    unsafe extern "system" fn visit(window: HWND, param: LPARAM) -> BOOL {
        let search = &mut *(param.0 as *mut Search);

        if !IsWindowVisible(window).as_bool() {
            return BOOL(1);
        }

        let mut pid: u32 = 0;
        GetWindowThreadProcessId(window, Some(&mut pid));
        if !search.pids.contains(&pid) {
            return BOOL(1);
        }

        let length = GetWindowTextLengthW(window);
        if length <= 0 {
            return BOOL(1);
        }
        let mut buffer = vec![0u16; length as usize + 1];
        let written = GetWindowTextW(window, &mut buffer);
        if written <= 0 {
            return BOOL(1);
        }
        let title = String::from_utf16_lossy(&buffer[..written as usize]);

        // "Weekly sync | Microsoft Teams" -> "Weekly sync". A window titled
        // only with the app name tells us nothing, so keep looking.
        let cleaned = title
            .split('|')
            .next()
            .unwrap_or(&title)
            .trim()
            .trim_end_matches('-')
            .trim()
            .to_string();

        if cleaned.is_empty() || cleaned.eq_ignore_ascii_case("Microsoft Teams") {
            return BOOL(1);
        }

        search.found = Some(cleaned);
        BOOL(0)
    }

    let pids: Vec<u32> = system
        .processes()
        .iter()
        .filter(|(_, process)| {
            let name = process.name();
            name == "MSTeams" || name == "Teams" || process.cmd().join(" ").contains("Microsoft Teams")
        })
        .map(|(pid, _)| pid.as_u32())
        .collect();

    if pids.is_empty() {
        return None;
    }

    let mut search = Search { pids, found: None };
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize));
    }
    search.found
}

#[cfg(not(target_os = "windows"))]
pub fn meeting_window_title(_system: &System) -> Option<String> {
    None
}

/// Words that mark a window title as the name of a meeting rather than of a
/// person. Kept deliberately short: a false "this is a person" costs a wrong
/// label, so the test errs towards saying no.
const MEETING_WORDS: [&str; 22] = [
    "meeting", "call", "sync", "standup", "stand-up", "daily", "weekly", "monthly", "review",
    "retro", "planning", "kickoff", "demo", "workshop", "interview", "spotkanie", "rozmowa",
    "zebranie", "narada", "prezentacja", "szkolenie", "status",
];

/// Prefixes Teams puts in front of a person's name in a one-to-one call.
const CALL_PREFIXES: [&str; 6] = [
    "call with ", "meeting with ", "chat with ", "rozmowa z ", "spotkanie z ", "połączenie z ",
];

/// Read a window title as the name of the person on the other end, when it
/// plausibly is one.
///
/// In a one-to-one call Teams titles the window with the other participant;
/// in a scheduled meeting it uses the meeting's name. Nothing in the title
/// distinguishes the two, so this applies a deliberately strict test: two or
/// three capitalised words, no digits, and none of the words that show up in
/// meeting names. Anything else returns None and the transcript keeps the
/// neutral "Others" label.
pub fn other_party_from_title(title: &str) -> Option<String> {
    let mut candidate = title.trim().to_string();

    let lowered = candidate.to_lowercase();
    for prefix in CALL_PREFIXES {
        if lowered.starts_with(prefix) {
            candidate = candidate[prefix.len()..].trim().to_string();
            break;
        }
    }

    if candidate.is_empty() || candidate.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }

    let lowered = candidate.to_lowercase();
    if MEETING_WORDS.iter().any(|word| lowered.contains(word)) {
        return None;
    }

    let words: Vec<&str> = candidate.split_whitespace().collect();
    if words.len() < 2 || words.len() > 3 {
        return None;
    }

    let looks_like_name = words.iter().all(|word| {
        let mut chars = word.chars();
        match chars.next() {
            Some(first) => first.is_uppercase() && word.chars().count() >= 2,
            None => false,
        }
    });

    if looks_like_name {
        Some(candidate)
    } else {
        None
    }
}

#[cfg(target_os = "windows")]
fn is_teams_in_meeting(system: &System) -> bool {
    if !is_teams_running(system) {
        return false;
    }
    app_holding_mic("teams")
}

struct MeetingDetector {
    system: System,
    previously_detected: HashSet<String>,
    /// Per-app cooldown tracking: app_name → last notification time
    cooldowns: HashMap<String, Instant>,
    /// Counts consecutive polls where a previously-detected app was NOT seen.
    /// Once this reaches LEAVE_GRACE_POLLS, we actually remove it.
    absent_counts: HashMap<String, u8>,
}

impl MeetingDetector {
    fn new() -> Self {
        Self {
            system: System::new_all(),
            previously_detected: HashSet::new(),
            cooldowns: HashMap::new(),
            absent_counts: HashMap::new(),
        }
    }

    /// Title of the meeting window, if one can be read.
    fn window_title(&self) -> Option<String> {
        meeting_window_title(&self.system)
    }

    /// Poll running processes. Returns (newly detected, just ended) meeting
    /// app names. New ones are filtered by the notification cooldown; ended
    /// ones never are, otherwise an automatic recording could be left running.
    fn poll(&mut self) -> (Vec<String>, Vec<String>) {
        self.system.refresh_processes();

        let mut current_raw: HashSet<String> = HashSet::new();

        if is_zoom_in_meeting(&self.system) {
            current_raw.insert("Zoom".to_string());
        }
        if is_teams_in_meeting(&self.system) {
            current_raw.insert("Microsoft Teams".to_string());
        }

        // Apply grace period: apps that were previously detected but aren't
        // in current_raw need to be absent for LEAVE_GRACE_POLLS consecutive
        // polls before we actually remove them.
        let mut current_meeting_apps = current_raw.clone();

        for app in &self.previously_detected {
            if !current_raw.contains(app) {
                let count = self.absent_counts.entry(app.clone()).or_insert(0);
                *count += 1;
                if *count < LEAVE_GRACE_POLLS {
                    debug!("{} not detected this poll ({}/{}), keeping in detected set",
                        app, count, LEAVE_GRACE_POLLS);
                    current_meeting_apps.insert(app.clone());
                } else {
                    info!("{} absent for {} polls, marking as left meeting", app, count);
                }
            }
        }

        // Clear absent counts for apps that ARE detected this poll
        for app in &current_raw {
            self.absent_counts.remove(app);
        }

        // Find apps that just entered a meeting (weren't detected in previous poll)
        let new_apps: Vec<String> = current_meeting_apps
            .difference(&self.previously_detected)
            .cloned()
            .collect();

        // ...and the ones that just left, so an automatic recording can stop.
        let ended_apps: Vec<String> = self
            .previously_detected
            .difference(&current_meeting_apps)
            .cloned()
            .collect();

        self.previously_detected = current_meeting_apps;

        // Filter out apps still in cooldown
        let now = Instant::now();
        let notifiable: Vec<String> = new_apps
            .into_iter()
            .filter(|app| {
                if let Some(last_time) = self.cooldowns.get(app) {
                    if now.duration_since(*last_time) < NOTIFICATION_COOLDOWN {
                        debug!("Skipping notification for {} (cooldown active)", app);
                        return false;
                    }
                }
                true
            })
            .collect();

        // Record cooldown for apps we're about to notify
        for app in &notifiable {
            self.cooldowns.insert(app.clone(), now);
        }

        (notifiable, ended_apps)
    }
}

/// Show the floating top-right popup window (Granola-style card). This is a
/// custom Tauri window — not a real macOS notification — so it doesn't need
/// `UNUserNotificationCenter` permission and works in `tauri dev`.
fn show_corner_notification(app_handle: &AppHandle, app_name: &str) {
    crate::engine::meeting_popup::show_meeting_popup(app_handle.clone(), app_name.to_string());
}

/// Spawns a background thread that polls for meeting processes and emits
/// events when a new meeting is detected. Call once from setup().
pub fn start_meeting_detection(app_handle: AppHandle) {
    std::thread::spawn(move || {
        info!("Meeting detection thread started");

        // Wait for the frontend to fully mount before polling,
        // otherwise the first event fires before the listener is ready.
        std::thread::sleep(Duration::from_secs(10));

        let mut detector = MeetingDetector::new();

        loop {
            if !MEETING_DETECTION_ENABLED.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }

            let (new_apps, ended_apps) = detector.poll();

            for app_name in &new_apps {
                info!("Meeting detected on: {}", app_name);

                // Silent capture: start recording without asking. The popup
                // and banner below are skipped entirely in this mode.
                if crate::auto_capture_enabled(&app_handle) {
                    // Name the recording after the meeting when the window
                    // title offers one: "Weekly sync" reads better than
                    // "Microsoft Teams" in a folder of transcripts.
                    let title = detector.window_title();
                    let label = match &title {
                        Some(title) => format!("{} ({})", title, app_name),
                        None => app_name.clone(),
                    };
                    // In a one-to-one call the window title is the other
                    // person, and naming them beats a generic "Others".
                    let other_party = title.as_deref().and_then(other_party_from_title);
                    crate::auto_record_start(app_handle.clone(), label, other_party);
                    continue;
                }

                // Quiet macOS corner notification (no focus steal).
                show_corner_notification(&app_handle, app_name);

                // In-app banner via frontend event (independent path —
                // shown if Platypus is already focused).
                if let Some(window) = app_handle.get_window("main") {
                    let _ = window.emit("meeting-detected", app_name.clone());
                }
            }

            for app_name in &ended_apps {
                info!("Meeting ended on: {}", app_name);
                crate::auto_record_stop(app_handle.clone(), app_name.clone());
            }

            std::thread::sleep(Duration::from_secs(5));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::other_party_from_title;

    #[test]
    fn reads_a_name_from_a_one_to_one_call() {
        assert_eq!(
            other_party_from_title("Jan Kowalski"),
            Some("Jan Kowalski".to_string())
        );
        assert_eq!(
            other_party_from_title("Call with Anna Nowak"),
            Some("Anna Nowak".to_string())
        );
    }

    #[test]
    fn refuses_meeting_names() {
        assert_eq!(other_party_from_title("Weekly sync"), None);
        assert_eq!(other_party_from_title("Spotkanie zespolu"), None);
        assert_eq!(other_party_from_title("Sprint planning 12"), None);
    }

    #[test]
    fn refuses_anything_that_is_not_shaped_like_a_name() {
        assert_eq!(other_party_from_title("Kowalski"), None);
        assert_eq!(other_party_from_title("a b"), None);
        assert_eq!(other_party_from_title(""), None);
    }
}
