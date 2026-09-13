// Prevents additional console window on Windows in release!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::env;
use std::sync::Arc;

use lazy_static::lazy_static;
use log::info;
use rusqlite::Connection;
use serde_derive::Serialize;
use tauri::utils::config::AppUrl;
use tauri::SystemTray;
use tauri::{AppHandle, Manager, State, SystemTrayEvent, WindowUrl};
use tauri::{CustomMenuItem, SystemTrayMenu};
use tauri_plugin_log::LogTarget;
use tokio::sync::Mutex;

use configuration::settings::Settings;

use crate::bootstrap::{fix_path_env, prerequisites, setup_directories};
use crate::configuration::database;
use crate::configuration::database::drop_database_handle;
use crate::configuration::state::{AppState, ServiceAccess};
use crate::engine::chat_engine::{name_conversation, send_prompt_to_llm};
use crate::engine::chat_engine_openai::{generate_conversation_name, send_prompt_to_openai};
use crate::engine::chat_engine_gemini::{name_conversation_gemini, send_prompt_to_gemini};
use crate::engine::chat_engine_local::{name_conversation_local, send_prompt_to_local};
use crate::engine::clean_up_engine::clean_up;
use crate::engine::document_cleanup_engine::{clean_up_document_with_llm, summarize_as_meeting_notes, generate_slides_from_document, draft_follow_up_email, generate_suggested_questions};
use crate::engine::podcast_generator::{generate_podcast_from_document, list_elevenlabs_voices};
use crate::engine::meeting_popup::{meeting_popup_dismiss, meeting_popup_start_recording};
use crate::engine::url_ingestion::ingest_url_command;
use crate::engine::similarity_search_engine::SyncSimilaritySearch;
use crate::entity::chat_item::{Chat, StoredMessage};
use crate::entity::permission::Permission;
use crate::entity::project::Project;
use crate::entity::setting::Setting;
use crate::permissions::permission_engine::init_permissions;
use crate::repository::chat_db_repository;
use crate::repository::chunk_repository::{save_chunks_for_document, get_chunk_full_text};
use crate::repository::permissions_repository::{get_permissions, update_permission};
use crate::repository::project_repository::{
    delete_project, fetch_all_projects, add_blank_document, save_project, update_project, get_activity_text_from_project, get_activity_plain_text, get_project_id_for_document, update_activity_text, update_activity_name, delete_project_document, ensure_unassigned_project, move_document_to_project, get_all_documents, search_documents_by_content,
};
use crate::repository::settings_repository::{get_setting, get_settings, insert_or_update_setting};
use tauri_plugin_autostart::MacosLauncher;

mod bootstrap;
mod configuration;
mod engine;
mod entity;
pub mod permissions;
mod repository;

#[derive(Clone, Serialize)]
struct Payload {
    data: bool,
}

#[cfg(debug_assertions)]
const USE_LOCALHOST_SERVER: bool = false;
#[cfg(not(debug_assertions))]
const USE_LOCALHOST_SERVER: bool = true;

lazy_static! {
    static ref HNSW: SyncSimilaritySearch = Arc::new(Mutex::new(None));
    static ref WHISPER_ENGINE: Arc<std::sync::Mutex<Option<crate::engine::whisper_engine::WhisperEngine>>> =
        Arc::new(std::sync::Mutex::new(None));
    static ref ACCUMULATED_TRANSCRIPT: Arc<std::sync::Mutex<String>> =
        Arc::new(std::sync::Mutex::new(String::new()));
}

//#[cfg(any(target_os = "macos"))]
//static ACCESSIBILITY_PERMISSIONS_GRANTED: AtomicBool = AtomicBool::new(false);

#[tokio::main]
async fn main() {
    let port = 5173;
    // Portable mode: redirect WebView2 storage next to the executable before
    // any window exists.
    configuration::portable::prepare_environment();

    let mut builder = tauri::Builder::default().plugin(tauri_plugin_oauth::init());

    fix_path_env::fix_all_vars().expect("Failed to load env");
    let tray = build_system_tray();

    let mut context = tauri::generate_context!();

    let url = format!("http://localhost:{}", port).parse().unwrap();
    let window_url = WindowUrl::External(url);

    if USE_LOCALHOST_SERVER == true {
        context.config_mut().build.dist_dir = AppUrl::Url(window_url.clone());
        context.config_mut().build.dev_path = AppUrl::Url(window_url.clone());
        builder = builder.plugin(tauri_plugin_localhost::Builder::new(port).build());
    }

    // Release builds have no console, so also write logs to a file: next to
    // the executable when portable, otherwise the OS log directory.
    let mut log_targets = vec![LogTarget::Stdout, LogTarget::Webview];
    match configuration::portable::logs_dir() {
        Some(dir) => log_targets.push(LogTarget::Folder(dir)),
        None => log_targets.push(LogTarget::LogDir),
    }

    builder
        .plugin(
            tauri_plugin_log::Builder::default()
                .targets(log_targets)
                .level_for("hnsw_rs", log::LevelFilter::Warn)
                .level_for("html5ever", log::LevelFilter::Warn)
                .level_for("selectors", log::LevelFilter::Warn)
                .level_for("tao", log::LevelFilter::Warn)
                .build(),
        )
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .plugin(tauri_plugin_oauth::init())
        .plugin(tauri_plugin_positioner::init())
        .system_tray(tray)
        .on_system_tray_event(|app, event| match event {
            // Ensure the window is toggled when the tray icon is clicked
            SystemTrayEvent::LeftClick { .. } => {
                let window = app.get_window("main").unwrap();
                if window.is_visible().unwrap() {
                    window.hide().unwrap();
                } else {
                    window.show().unwrap();
                    window.set_focus().unwrap();
                }
            }
            SystemTrayEvent::MenuItemClick { id, .. } => match id.as_str() {
                "start_stop_recording" => {
                    let wrapped_window = app.get_window("main");
                    if let Some(window) = wrapped_window {
                        window
                            .emit("toggle_recording", Payload { data: true })
                            .unwrap();
                    }
                }
                "quit" => {
                    std::process::exit(0);
                }
                _ => {}
            },
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            update_settings,
            get_latest_settings,
            send_prompt_to_llm,
            send_prompt_to_openai,
            send_prompt_to_gemini,
            send_prompt_to_local,
            generate_conversation_name,
            name_conversation_gemini,
            name_conversation_local,
            name_conversation,
            create_chat,
            get_all_chats,
            create_message,
            get_messages_by_chat_id,
            update_chat_name,
            update_app_permissions,
            get_app_permissions,
            get_projects,
            save_app_project,
            update_app_project,
            delete_app_project,
            delete_chat,
            get_chunk_text,
            prompt_for_accessibility_permissions,
            get_app_project_activity_text,
            update_project_activity_text,
            vectorize_document_chunks,
            add_project_blank_activity,
            update_project_activity_name,
            delete_project_activity,
            ensure_unassigned_activity,
            update_project_activity_content,
            get_app_project_activity_plain_text,
            get_all_project_documents,
            start_audio_recording,
            stop_audio_recording,
            read_audio_file,
            transcribe_audio,
            extract_document_text,
            ingest_url_command,
            clean_up_document_with_llm,
            summarize_as_meeting_notes,
            draft_follow_up_email,
            generate_suggested_questions,
            search_documents_content,
            generate_slides_from_document,
            generate_podcast_from_document,
            list_elevenlabs_voices,
            check_whisper_model,
            download_whisper_model,
            init_whisper_model,
            get_transcript,
            meeting_popup_dismiss,
            meeting_popup_start_recording,
        ])
        .manage(AppState {
            db: Default::default(),
        })
        .on_window_event(|event| match event.event() {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                event.window().hide().unwrap(); // Hide window on close
            }
            _ => {}
        })
        .setup(move |app| {
            let args: Vec<String> = env::args().collect();
            let should_start_minimized = args.contains(&"--minimized".to_string());

            let window = app.get_window("main").unwrap();

            if should_start_minimized {
                window.hide().unwrap();
            } else {
                window.show().unwrap();
            }

            let app_handle = app.handle();
            let app_data_dir = configuration::portable::app_data_dir_or_panic(&app_handle);
            info!(
                "Portable mode: {} (data dir: {})",
                configuration::portable::is_portable(),
                app_data_dir.display()
            );
            // The database must exist before any setting is read: `.db()`
            // unwraps the connection, so reading a setting before this line
            // panics on startup.
            setup_keypress_listener(&app_handle);
            seed_default_settings(&app_handle);

            // Screenshot/task-mining scaffolding is off unless asked for.
            let task_mining = app_handle
                .db(|db| get_setting(db, "task_mining_enabled"))
                .map(|setting| setting.setting_value)
                .unwrap_or_default()
                .eq_ignore_ascii_case("true");
            if task_mining {
                let _ = setup_directories::setup_dirs(app_data_dir.to_str().unwrap());
            }
            prerequisites::check_and_install_prerequisites(
                app_handle
                    .path_resolver()
                    .resource_dir()
                    .unwrap()
                    .to_str()
                    .unwrap(),
            );
            if task_mining {
                clean_up(app_data_dir.clone());
            }

            // A .partial.md left over means a recording that never stopped
            // cleanly; turn it into a transcript rather than leaving it.
            engine::transcript_export::recover_interrupted(&app_handle);

            // `Platypus.exe --self-test` checks the capture chain and exits,
            // so a freshly installed machine can be verified without waiting
            // for a real meeting.
            if args.contains(&"--self-test".to_string()) {
                let handle = app_handle.clone();
                tauri::async_runtime::spawn(async move {
                    // Give the whisper engine a chance to load first.
                    let model_id = handle
                        .db(|db| get_setting(db, "whisper_model"))
                        .map(|setting| setting.setting_value)
                        .unwrap_or_default();
                    let model_id = if model_id.trim().is_empty() {
                        "large-v3".to_string()
                    } else {
                        model_id
                    };
                    if engine::whisper_engine::is_model_downloaded(&model_id) {
                        if let Ok(loaded) = engine::whisper_engine::WhisperEngine::load(&model_id) {
                            *WHISPER_ENGINE.lock().unwrap() = Some(loaded);
                        }
                    }

                    let report = engine::self_test::run(&handle).await;
                    println!("{}", report);

                    if let Some(dir) = configuration::portable::app_data_dir(&handle) {
                        let path = dir.join("self-test.md");
                        match std::fs::write(&path, &report) {
                            Ok(()) => info!("Self-test report written to {}", path.display()),
                            Err(err) => log::warn!("Could not write the self-test report: {}", err),
                        }
                    }

                    handle.exit(0);
                });
            }

            let language = app_handle
                .db(|db| get_setting(db, "transcription_language"))
                .map(|setting| setting.setting_value)
                .unwrap_or_default();
            engine::whisper_engine::set_language(&language);

            let separate = app_handle
                .db(|db| get_setting(db, "separate_speaker_streams"))
                .map(|setting| setting.setting_value)
                .unwrap_or_default();
            if !separate.trim().is_empty() {
                SEPARATE_SPEAKERS.store(
                    separate.eq_ignore_ascii_case("true"),
                    std::sync::atomic::Ordering::SeqCst,
                );
            }
            info!(
                "Per-speaker transcription: {}",
                SEPARATE_SPEAKERS.load(std::sync::atomic::Ordering::SeqCst)
            );

            // System audio capture is read once at startup; changing it takes
            // effect on the next run.
            let capture_system_audio = app_handle
                .db(|db| get_setting(db, "capture_system_audio"))
                .map(|setting| setting.setting_value)
                .unwrap_or_default();
            engine::audio_engine::CAPTURE_SYSTEM_AUDIO.store(
                !capture_system_audio.eq_ignore_ascii_case("false"),
                std::sync::atomic::Ordering::SeqCst,
            );

            // Load meeting detection setting from DB and start the detector thread
            let detection_enabled = app_handle.db(|db| {
                get_setting(db, "meeting_detection_enabled")
                    .map(|s| s.setting_value == "true")
                    .unwrap_or(false)
            });
            engine::meeting_detector::MEETING_DETECTION_ENABLED
                .store(detection_enabled, std::sync::atomic::Ordering::Relaxed);
            engine::meeting_detector::start_meeting_detection(app_handle.clone());

            init_app_permissions(app_handle);
            Ok(())
        })
        .run(context)
        .expect("error while running tauri application");
    drop_database_handle().await;
}

fn build_system_tray() -> SystemTray {
    let quit = CustomMenuItem::new("quit".to_string(), "Quit");
    let tray_menu = SystemTrayMenu::new()
        .add_item(quit);
    SystemTray::new().with_menu(tray_menu)
}

fn setup_keypress_listener(app_handle: &AppHandle) {
    let app_state: State<AppState> = app_handle.state();

    let db: Connection =
        database::initialize_database(&app_handle).expect("Database initialization failed!");
    *app_state.db.lock().unwrap() = Some(db);
}

/// Seed the settings a headless capture build depends on, the first time it
/// runs. Only missing keys are written - anything the user has already chosen
/// is left alone.
fn seed_default_settings(app_handle: &AppHandle) {
    const DEFAULTS: [(&str, &str); 9] = [
        // Transcribe on this machine: an unattended capture cannot depend on
        // an API key being present.
        ("use_local_transcription", "true"),
        ("whisper_model", "large-v3-turbo"),
        ("meeting_detection_enabled", "true"),
        ("auto_capture_meetings", "true"),
        // Record the speakers as well as the microphone - without it a call
        // transcript is only your own half of the conversation.
        ("capture_system_audio", "true"),
        // "auto" detects per chunk; set a code like "pl" when you know what
        // will be spoken - detection costs accuracy on short chunks.
        ("transcription_language", "auto"),
        // Pull decisions and action items out of each meeting afterwards.
        // "local" uses Ollama and keeps everything on this machine; "claude",
        // "openai" and "gemini" are better at it; "off" disables the pass.
        ("post_meeting_analysis", "local"),
        ("vectorization_enabled", "true"),
        // 20 chunks of 4000 characters is ~25k tokens per question, which
        // overflows a local model's context and dilutes retrieval for a
        // hosted one.
        ("rag_top_k", "12"),
    ];

    for (key, value) in DEFAULTS {
        let existing = app_handle
            .db(|db| get_setting(db, key))
            .map(|setting| setting.setting_value)
            .unwrap_or_default();

        if existing.trim().is_empty() {
            let result = app_handle.db(|db| {
                insert_or_update_setting(
                    db,
                    Setting {
                        setting_key: key.to_string(),
                        setting_value: value.to_string(),
                    },
                )
            });
            match result {
                Ok(()) => info!("Seeded default setting {} = {}", key, value),
                Err(err) => log::warn!("Could not seed setting {}: {}", key, err),
            }
        }
    }
}

#[tauri::command]
fn get_latest_settings(app_handle: AppHandle) -> Result<Vec<Setting>, ()> {
    let settings = app_handle.db(|db| get_settings(db).unwrap());
    return Ok(settings);
}

#[tauri::command]
async fn update_settings(app_handle: AppHandle, settings: Settings) {
    info!("update_settings: {:?}", settings);
    app_handle.db(|db| {
        insert_or_update_setting(
            db,
            Setting {
                setting_key: String::from("interval"),
                setting_value: format!("{}", settings.interval),
            },
        )
        .unwrap();
        insert_or_update_setting(
            db,
            Setting {
                setting_key: String::from("is_dev_mode"),
                setting_value: format!("{}", settings.is_dev_mode),
            },
        )
        .unwrap();
        insert_or_update_setting(
            db,
            Setting {
                setting_key: String::from("auto_start"),
                setting_value: format!("{}", settings.auto_start),
            },
        )
        .unwrap();
        insert_or_update_setting(
            db,
            Setting {
                setting_key: String::from("api_choice"),
                setting_value: format!("{}", settings.api_choice),
            },
        )
        .unwrap();
        insert_or_update_setting(
            db,
            Setting {
                setting_key: String::from("api_key_claude"),
                setting_value: format!("{}", settings.api_key_claude),
            },
        )
        .unwrap();
        insert_or_update_setting(
            db,
            Setting {
                setting_key: String::from("api_key_open_ai"),
                setting_value: format!("{}", settings.api_key_open_ai),
            },
        )
        .unwrap();
        insert_or_update_setting(
            db,
            Setting {
                setting_key: String::from("api_key_gemini"),
                setting_value: format!("{}", settings.api_key_gemini),
            },
        )
        .unwrap();
        insert_or_update_setting(
            db,
            Setting {
                setting_key: String::from("local_model_url"),
                setting_value: format!("{}", settings.local_model_url),
            },
        )
        .unwrap();
        insert_or_update_setting(
            db,
            Setting {
                setting_key: String::from("vectorization_enabled"),
                setting_value: format!("{}", settings.vectorization_enabled),
            },
        )
        .unwrap();
        insert_or_update_setting(
            db,
            Setting {
                setting_key: String::from("rag_top_k"),
                setting_value: format!("{}", settings.rag_top_k),
            },
        )
        .unwrap();
        insert_or_update_setting(
            db,
            Setting {
                setting_key: String::from("meeting_detection_enabled"),
                setting_value: format!("{}", settings.meeting_detection_enabled),
            },
        )
        .unwrap();
        insert_or_update_setting(
            db,
            Setting {
                setting_key: String::from("model_claude"),
                setting_value: settings.model_claude.clone(),
            },
        )
        .unwrap();
        insert_or_update_setting(
            db,
            Setting {
                setting_key: String::from("model_openai"),
                setting_value: settings.model_openai.clone(),
            },
        )
        .unwrap();
        insert_or_update_setting(
            db,
            Setting {
                setting_key: String::from("model_gemini"),
                setting_value: settings.model_gemini.clone(),
            },
        )
        .unwrap();
        insert_or_update_setting(
            db,
            Setting {
                setting_key: String::from("use_local_transcription"),
                setting_value: format!("{}", settings.use_local_transcription),
            },
        )
        .unwrap();
        insert_or_update_setting(
            db,
            Setting {
                setting_key: String::from("whisper_model"),
                setting_value: settings.whisper_model.clone(),
            },
        )
        .unwrap();
        insert_or_update_setting(
            db,
            Setting {
                setting_key: String::from("api_key_elevenlabs"),
                setting_value: settings.api_key_elevenlabs.clone(),
            },
        )
        .unwrap();
    });

    // Update the runtime flag so the detection loop picks up the change immediately
    engine::meeting_detector::MEETING_DETECTION_ENABLED
        .store(settings.meeting_detection_enabled, std::sync::atomic::Ordering::Relaxed);
}

#[tauri::command]
fn init_app_permissions(app_handle: AppHandle) {
    init_permissions(app_handle);
}

#[tauri::command]
fn update_app_permissions(app_handle: AppHandle, app_path: String, allow: bool) {
    app_handle.db(|database| {
        update_permission(database, app_path, allow).expect("Failed to update permission");
    })
}

#[tauri::command]
fn get_app_permissions(app_handle: AppHandle) -> Result<Vec<Permission>, ()> {
    let permissions = app_handle.db(|database| get_permissions(database).unwrap());
    return Ok(permissions);
}

#[tauri::command]
fn get_projects(app_handle: AppHandle) -> Result<Vec<Project>, ()> {
    let projects = app_handle.db(|database| fetch_all_projects(database).unwrap());
    return Ok(projects);
}

#[tauri::command]
fn save_app_project(
    app_handle: AppHandle,
    name: &str,
    activities: Vec<i64>,
) -> Result<Vec<i64>, ()> {
    app_handle.db(|database| save_project(database, name, &activities).unwrap());
    return Ok(activities);
}

#[tauri::command]
fn update_app_project(
    app_handle: AppHandle,
    id: i64,
    name: &str,
    activities: Vec<i64>,
) -> Result<Vec<i64>, ()> {
    app_handle.db(|database| update_project(database, id, name, &activities).unwrap());
    return Ok(activities);
}

#[tauri::command]
fn delete_app_project(app_handle: AppHandle, project_id: i64) -> Result<i64, ()> {
    app_handle.db(|database| delete_project(database, project_id).unwrap());
    return Ok(project_id);
}

#[tauri::command]
fn create_chat(app_handle: AppHandle, name: &str) -> Result<i64, String> {
    app_handle
        .db(|db| chat_db_repository::create_chat(db, name))
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn get_all_chats(app_handle: AppHandle) -> Result<Vec<Chat>, String> {
    app_handle
        .db(|db| chat_db_repository::get_all_chats(db))
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn create_message(
    app_handle: AppHandle,
    chat_id: i64,
    role: &str,
    content: &str,
    sources: Option<String>,
) -> Result<i64, String> {
    app_handle
        .db(|db| chat_db_repository::create_message(db, chat_id, role, content, sources.as_deref()))
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn get_messages_by_chat_id(
    app_handle: AppHandle,
    chat_id: i64,
) -> Result<Vec<StoredMessage>, String> {
    app_handle
        .db(|db| chat_db_repository::get_messages_by_chat_id(db, chat_id))
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn update_chat_name(app_handle: AppHandle, chat_id: i64, name: &str) -> Result<bool, String> {
    app_handle
        .db(|db| chat_db_repository::update_chat(db, chat_id, name))
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_chat(app_handle: AppHandle, chat_id: i64) -> Result<bool, String> {
    app_handle
        .db(|db| chat_db_repository::delete_chat(db, chat_id))
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn get_chunk_text(app_handle: AppHandle, chunk_id: i64) -> Result<Option<String>, String> {
    app_handle
        .db(|db| get_chunk_full_text(db, chunk_id))
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn get_app_project_activity_text(
    app_handle: AppHandle,
    project_id: i64,
    activity_id: i64,
) -> Result<String, ()> {
    // Properly propagate errors instead of unwrapping
    let text = app_handle.db(|database| {
        match get_activity_text_from_project(database, project_id, activity_id) {
            Ok(text) => Ok(text),
            Err(_) => Err(())  // Or provide more specific error information
        }
    })?;  // Propagate error with ? operator
    
    Ok(text)
}

#[tauri::command]
fn get_app_project_activity_plain_text(
    app_handle: AppHandle,
    activity_id: i64,
) -> Result<(String, String), String> {
    app_handle
        .db(|database| get_activity_plain_text(database, activity_id))
        .map_err(|e| e.to_string())
}

/// Get all documents across all projects for the "Add content to Platypus" modal
#[tauri::command]
fn get_all_project_documents(
    app_handle: AppHandle,
) -> Result<Vec<(i64, String, String, String)>, String> {
    app_handle
        .db(|database| get_all_documents(database))
        .map_err(|e| e.to_string())
}

/// Content search over document plain text; returns matching document IDs so
/// the notes list can include content hits alongside name matches.
#[tauri::command]
fn search_documents_content(
    app_handle: AppHandle,
    search_term: String,
) -> Result<Vec<i64>, String> {
    let term = search_term.trim().to_string();
    if term.is_empty() {
        return Ok(vec![]);
    }
    app_handle
        .db(|database| search_documents_by_content(database, &term))
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn update_project_activity_content(
    app_handle: AppHandle,
    document_id: i64,
    target_project_id: i64,
) -> Result<(), String> {
    app_handle
        .db(|database| {
            move_document_to_project(database, document_id, target_project_id)
                .map_err(|e| e.to_string())
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn update_project_activity_text(
    app_handle: AppHandle,
    activity_id: i64,
    text: &str,
) -> Result<(), String> {
    // Update the document text (this also generates plain_text)
    app_handle
        .db(|db| update_activity_text(db, activity_id, text))
        .map_err(|e| e.to_string())?;
    
    // Only chunk if vectorization is enabled
    let vectorization_enabled = app_handle
        .db(|db| get_setting(db, "vectorization_enabled"))
        .map(|s| s.setting_value == "true")
        .unwrap_or(false);

    if vectorization_enabled {
        let (project_id, plain_text) = app_handle
            .db(|db| {
                let project_id = get_project_id_for_document(db, activity_id)?;
                let (_, plain_text) = get_activity_plain_text(db, activity_id)?;
                Ok::<(i64, String), rusqlite::Error>((project_id, plain_text))
            })
            .map_err(|e| e.to_string())?;

        app_handle
            .db(|db| save_chunks_for_document(db, activity_id, project_id, &plain_text))
            .map_err(|e| e.to_string())?;

        info!("Document {} updated and chunked", activity_id);
    } else {
        info!("Document {} updated (vectorization disabled, skipping chunking)", activity_id);
    }
    Ok(())
}

/// Vectorize all unvectorized chunks for a document
/// Called after document is saved when vectorization is enabled
/// Uses per-project vector indices for proper scoping
#[tauri::command]
async fn vectorize_document_chunks(
    app_handle: AppHandle,
    document_id: i64,
) -> Result<i32, String> {
    use crate::repository::chunk_repository::mark_chunk_as_vectorized;
    use crate::engine::project_vector_engine::{add_chunk_to_project_vectors, sync_project_vectors};
    use log::{info, error};
    
    // Check if vectorization is enabled
    let vectorization_enabled = app_handle
        .db(|db| get_setting(db, "vectorization_enabled"))
        .map(|s| s.setting_value == "true")
        .unwrap_or(false);
    
    if !vectorization_enabled {
        info!("Vectorization disabled, skipping for document {}", document_id);
        return Ok(0);
    }
    
    // Get OpenAI API key
    let api_key = app_handle
        .db(|db| get_setting(db, "api_key_open_ai"))
        .map(|s| s.setting_value)
        .unwrap_or_default();
    
    if api_key.is_empty() {
        info!("No OpenAI API key, skipping vectorization for document {}", document_id);
        return Ok(0);
    }
    
    // Get project_id for the document
    let project_id = app_handle
        .db(|db| get_project_id_for_document(db, document_id))
        .map_err(|e| e.to_string())?;
    
    // Get unvectorized chunks for this document
    let chunks = app_handle
        .db(|db| {
            let mut stmt = db.prepare(
                "SELECT id, document_id, project_id, chunk_index, chunk_text, is_vectorized
                 FROM document_chunks 
                 WHERE document_id = ?1 AND is_vectorized = 0"
            )?;
            
            let chunks: Vec<crate::repository::chunk_repository::DocumentChunk> = stmt.query_map(
                rusqlite::params![document_id],
                |row| {
                    Ok(crate::repository::chunk_repository::DocumentChunk {
                        id: row.get(0)?,
                        document_id: row.get(1)?,
                        project_id: row.get(2)?,
                        chunk_index: row.get(3)?,
                        chunk_text: row.get(4)?,
                        is_vectorized: row.get::<_, i32>(5)? == 1,
                    })
                }
            )?.collect::<Result<Vec<_>, _>>()?;
            
            Ok::<Vec<crate::repository::chunk_repository::DocumentChunk>, rusqlite::Error>(chunks)
        })
        .map_err(|e| e.to_string())?;
    
    if chunks.is_empty() {
        info!("No chunks to vectorize for document {}", document_id);
        return Ok(0);
    }
    
    info!("Vectorizing {} chunks for document {} in project {}", chunks.len(), document_id, project_id);
    
    let mut vectorized_count = 0;
    
    for chunk in chunks {
        // Add to project-specific vector index
        if let Err(e) = add_chunk_to_project_vectors(
            &app_handle,
            project_id,
            chunk.id,
            &chunk.chunk_text,
            &api_key
        ).await {
            error!("Failed to vectorize chunk {}: {}", chunk.id, e);
            continue;
        }
        
        // Mark as vectorized in DB
        if let Err(e) = app_handle.db(|db| mark_chunk_as_vectorized(db, chunk.id)) {
            error!("Failed to mark chunk {} as vectorized: {}", chunk.id, e);
            continue;
        }
        
        vectorized_count += 1;
    }
    
    // Sync project's vector index to disk
    if let Err(e) = sync_project_vectors(&app_handle, project_id).await {
        error!("Failed to sync project {} vector index: {}", project_id, e);
    }
    
    info!("Vectorized {} chunks for document {} in project {}", vectorized_count, document_id, project_id);
    Ok(vectorized_count)
}

#[tauri::command]
fn add_project_blank_activity(
    app_handle: AppHandle,
    project_id: i64,
) -> Result<i64, String> {
    app_handle
        .db(|db| add_blank_document(db, project_id))
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn ensure_unassigned_activity(app_handle: AppHandle) -> Result<i64, String> {
  app_handle
    .db(|db| {
      // First ensure unassigned project exists
      let unassigned_project_id = ensure_unassigned_project(db)?;
      // Then add blank document to it
      add_blank_document(db, unassigned_project_id)
    })
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn update_project_activity_name(
    app_handle: AppHandle,
    activity_id: i64,
    name: &str,
) -> Result<(), String> {
    app_handle
        .db(|db| update_activity_name(db, activity_id, name))
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_project_activity(
    app_handle: AppHandle,
    activity_id: i64,
) -> Result<(), String> {
    app_handle
        .db(|db| delete_project_document(db, activity_id))
        .map_err(|e| e.to_string())
}

#[cfg(target_os = "macos")]
#[tauri::command]
fn prompt_for_accessibility_permissions() {
    // No-op - accessibility permissions no longer needed
}

#[cfg(not(target_os = "macos"))]
#[tauri::command]
fn prompt_for_accessibility_permissions() {
    // No-op for non-macOS platforms
}

// Audio recording commands — dual mode (file-based for OpenAI, buffer for local Whisper)
#[tauri::command]
async fn start_audio_recording(app_handle: AppHandle, use_local: bool) -> Result<String, String> {
    if use_local {
        crate::engine::audio_engine::start_recording_local().await?;
        engine::whisper_engine::reset_detected_language();
        // Clear accumulated transcript
        {
            let mut t = ACCUMULATED_TRANSCRIPT.lock().unwrap();
            t.clear();
        }
        {
            let mut last = LAST_SPEAKER.lock().unwrap();
            *last = None;
        }
        {
            // A recording started by hand has no meeting context to name.
            let mut label = OTHERS_LABEL.lock().unwrap();
            *label = "Others".to_string();
        }
        // Spawn the realtime transcription loop
        let handle = app_handle.clone();
        tokio::spawn(async move {
            realtime_transcription_loop(handle).await;
        });
        Ok("local".to_string())
    } else {
        crate::engine::audio_engine::start_recording().await
    }
}

#[tauri::command]
async fn stop_audio_recording(app_handle: AppHandle, use_local: bool) -> Result<String, String> {
    if use_local {
        crate::engine::audio_engine::stop_recording_local().await?;
        // Give the realtime loop a moment to finish
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        // Process any remaining samples
        let remaining = crate::engine::audio_engine::drain_all_samples();
        if !remaining.is_empty() {
            if let Some(text) = process_and_transcribe_chunk(&remaining) {
                let mut t = ACCUMULATED_TRANSCRIPT.lock().unwrap();
                append_tail(&mut t, 0, &text);
            }
        }
        // Emit final transcript
        let final_text = {
            let t = ACCUMULATED_TRANSCRIPT.lock().unwrap();
            t.clone()
        };
        if let Some(w) = app_handle.get_window("main") {
            let _ = w.emit("transcript-update", serde_json::json!({
                "text": final_text,
                "is_final": true
            }));
        }
        Ok(final_text)
    } else {
        crate::engine::audio_engine::stop_recording().await
    }
}

// ---------------------------------------------------------------------------
// Unattended meeting capture
//
// When meeting detection fires, recording starts on its own - no window, no
// popup, no click - and stops when the meeting ends. The transcript is written
// to `<app data>/transcripts` as Markdown so other tools can pick it up
// without going through the database.
// ---------------------------------------------------------------------------

static AUTO_RECORDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

lazy_static! {
    /// (source app, start time) of the recording in progress, if any.
    static ref AUTO_RECORD_META: Arc<std::sync::Mutex<Option<(String, chrono::DateTime<chrono::Local>)>>> =
        Arc::new(std::sync::Mutex::new(None));

    /// Speaker label used for the previous piece of transcript, so a new
    /// heading is only written when the side actually changes.
    static ref LAST_SPEAKER: Arc<std::sync::Mutex<Option<String>>> =
        Arc::new(std::sync::Mutex::new(None));

    /// What to call the far end of the call. A name when a one-to-one call
    /// gave us one, "Others" otherwise.
    static ref OTHERS_LABEL: Arc<std::sync::Mutex<String>> =
        Arc::new(std::sync::Mutex::new("Others".to_string()));

    /// Timed utterances of the recording in progress, for the JSON companion
    /// to the transcript.
    static ref AUTO_UTTERANCES: Arc<std::sync::Mutex<Vec<engine::transcript_export::Utterance>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
}

/// Capture meetings without asking. Defaults to on: a build that runs hidden
/// in the tray has nobody to ask.
pub(crate) fn auto_capture_enabled(app_handle: &AppHandle) -> bool {
    let value = app_handle
        .db(|db| get_setting(db, "auto_capture_meetings"))
        .map(|s| s.setting_value)
        .unwrap_or_default();

    match value.trim() {
        "" => true,
        other => other.eq_ignore_ascii_case("true"),
    }
}

pub(crate) fn auto_record_start(
    app_handle: AppHandle,
    app_name: String,
    other_party: Option<String>,
) {
    tauri::async_runtime::spawn(async move {
        use std::sync::atomic::Ordering;

        if engine::audio_engine::IS_RECORDING.load(Ordering::SeqCst) {
            info!("Auto-capture: already recording, ignoring {}", app_name);
            return;
        }

        if let Err(err) = engine::audio_engine::start_recording_local().await {
            log::warn!("Auto-capture: could not start recording: {}", err);
            return;
        }

        engine::whisper_engine::reset_detected_language();
        {
            let mut transcript = ACCUMULATED_TRANSCRIPT.lock().unwrap();
            transcript.clear();
        }
        {
            let mut last = LAST_SPEAKER.lock().unwrap();
            *last = None;
        }
        {
            let mut utterances = AUTO_UTTERANCES.lock().unwrap();
            utterances.clear();
        }
        {
            let mut label = OTHERS_LABEL.lock().unwrap();
            *label = other_party.clone().unwrap_or_else(|| "Others".to_string());
            if let Some(name) = &other_party {
                info!("Auto-capture: other party looks like {}", name);
            }
        }
        {
            let mut meta = AUTO_RECORD_META.lock().unwrap();
            *meta = Some((app_name.clone(), chrono::Local::now()));
        }
        AUTO_RECORDING.store(true, Ordering::SeqCst);
        info!("Auto-capture: recording started for {}", app_name);

        let handle = app_handle.clone();
        tauri::async_runtime::spawn(async move {
            realtime_transcription_loop(handle).await;
        });
    });
}

pub(crate) fn auto_record_stop(app_handle: AppHandle, app_name: String) {
    tauri::async_runtime::spawn(async move {
        use std::sync::atomic::Ordering;

        // Only stop what we started - a recording the user began by hand must
        // not be cut short because a meeting ended.
        if !AUTO_RECORDING.swap(false, Ordering::SeqCst) {
            return;
        }

        if let Err(err) = engine::audio_engine::stop_recording_local().await {
            log::warn!("Auto-capture: stop failed: {}", err);
        }

        // Let the realtime loop notice IS_RECORDING went false.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let remaining = engine::audio_engine::drain_all_samples();
        if !remaining.is_empty() {
            if let Some(text) = process_and_transcribe_chunk(&remaining) {
                let elapsed_ms = {
                    let meta = AUTO_RECORD_META.lock().unwrap();
                    meta.as_ref()
                        .map(|(_, started)| {
                            chrono::Local::now()
                                .signed_duration_since(*started)
                                .num_milliseconds()
                                .max(0) as u64
                        })
                        .unwrap_or(0)
                };
                let mut transcript = ACCUMULATED_TRANSCRIPT.lock().unwrap();
                append_tail(&mut transcript, elapsed_ms, &text);
                record_utterance(elapsed_ms, elapsed_ms, "Unknown", &text);
            }
        }

        let text = {
            let transcript = ACCUMULATED_TRANSCRIPT.lock().unwrap();
            transcript.clone()
        };

        let (source_app, started_at) = {
            let mut meta = AUTO_RECORD_META.lock().unwrap();
            meta.take()
                .unwrap_or_else(|| (app_name.clone(), chrono::Local::now()))
        };

        if text.trim().is_empty() {
            info!("Auto-capture: nothing transcribed for {}, no file written", source_app);
            return;
        }

        let other_party = {
            let label = OTHERS_LABEL.lock().unwrap().clone();
            if label == "Others" {
                None
            } else {
                Some(label)
            }
        };

        let ended_at = chrono::Local::now();
        match engine::transcript_export::write_markdown(
            &app_handle,
            &source_app,
            other_party.as_deref(),
            &text,
            started_at,
            ended_at,
        ) {
            Ok(path) => {
                info!("Auto-capture: transcript saved to {}", path.display());
                engine::transcript_export::discard_partial(&app_handle, started_at);

                let utterances = AUTO_UTTERANCES.lock().unwrap().clone();
                match engine::transcript_export::write_sidecar(
                    &path,
                    &source_app,
                    other_party.as_deref(),
                    &utterances,
                    started_at,
                    ended_at,
                ) {
                    Ok(sidecar) => {
                        info!("Auto-capture: timings saved to {}", sidecar.display());
                        analyse_meeting(
                            app_handle.clone(),
                            sidecar,
                            engine::transcript_export::meeting_id(&source_app, started_at),
                            text.clone(),
                        );
                    }
                    Err(err) => log::warn!("Auto-capture: could not write timings: {}", err),
                }

                if let Err(err) = engine::transcript_export::append_index(
                    &app_handle,
                    &path,
                    &source_app,
                    other_party.as_deref(),
                    &text,
                    started_at,
                    ended_at,
                ) {
                    log::warn!("Auto-capture: could not update the index: {}", err);
                }
            }
            Err(err) => log::warn!("Auto-capture: could not write transcript: {}", err),
        }

        // The file is what other tools read, but store the transcript as a
        // note too, otherwise the app itself cannot search or answer questions
        // about meetings it recorded.
        store_transcript_as_note(&app_handle, &source_app, &text, started_at);
    });
}

/// Ask a model to pull decisions and action items out of a finished meeting,
/// and fold the answer into the sidecar.
///
/// Runs after the transcript is on disk, and failure is logged rather than
/// propagated: a meeting that was captured but not analysed is a far better
/// outcome than losing it because a model was unavailable.
fn analyse_meeting(
    app_handle: AppHandle,
    sidecar: std::path::PathBuf,
    meeting_id: String,
    transcript: String,
) {
    tauri::async_runtime::spawn(async move {
        let provider = app_handle
            .db(|db| get_setting(db, "post_meeting_analysis"))
            .map(|setting| setting.setting_value)
            .unwrap_or_default();
        let provider = provider.trim();

        if provider.is_empty() || provider.eq_ignore_ascii_case("off") {
            return;
        }
        if transcript.split_whitespace().count() < 30 {
            info!("Auto-capture: transcript too short to analyse");
            return;
        }

        info!("Auto-capture: extracting decisions with provider {}", provider);
        match engine::document_cleanup_engine::extract_meeting_facts(
            &app_handle,
            &transcript,
            provider,
            None,
        )
        .await
        {
            Ok(output) => match engine::transcript_export::attach_analysis(&sidecar, &output) {
                Ok(analysis) => {
                    info!("Auto-capture: analysis added to {}", sidecar.display());
                    if let Err(err) = engine::transcript_export::append_index_analysis(
                        &app_handle,
                        &meeting_id,
                        &analysis,
                    ) {
                        log::warn!("Auto-capture: could not index the analysis: {}", err);
                    }
                }
                Err(err) => log::warn!("Auto-capture: could not attach analysis: {}", err),
            },
            Err(err) => log::warn!("Auto-capture: analysis failed: {}", err),
        }
    });
}

/// Save a captured transcript as a document in the Unassigned project.
fn store_transcript_as_note(
    app_handle: &AppHandle,
    source_app: &str,
    text: &str,
    started_at: chrono::DateTime<chrono::Local>,
) {
    let title = format!("{} - {}", source_app, started_at.format("%Y-%m-%d %H:%M"));
    let html = format!("<p>{}</p>", html_escape(text));

    let stored = app_handle.db(|db| -> Result<i64, rusqlite::Error> {
        let project_id = ensure_unassigned_project(db)?;
        let document_id = add_blank_document(db, project_id)?;
        update_activity_name(db, document_id, &title)?;
        update_activity_text(db, document_id, &html)?;
        Ok(document_id)
    });

    match stored {
        Ok(document_id) => info!("Auto-capture: transcript stored as document {}", document_id),
        Err(err) => log::warn!("Auto-capture: could not store transcript as note: {}", err),
    }
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[tauri::command]
fn read_audio_file(file_path: String) -> Result<Vec<u8>, String> {
    crate::engine::audio_engine::read_audio_file(&file_path)
}

#[tauri::command]
async fn transcribe_audio(
    app_handle: AppHandle,
    file_path: String,
) -> Result<String, String> {
    use crate::configuration::state::ServiceAccess;
    use crate::repository::settings_repository::get_setting;

    log::info!("Transcribing audio file: {}", file_path);

    // Get OpenAI API key from settings
    let setting = app_handle.db(|db| {
        get_setting(db, "api_key_open_ai").expect("Failed to get api_key_open_ai")
    });

    let openai_api_key = setting.setting_value;
    if openai_api_key.is_empty() {
        return Err("OpenAI API key is required for audio transcription".to_string());
    }

    // Transcribe using OpenAI Whisper
    let transcription = crate::engine::transcription_engine::transcribe_with_openai(
        &file_path,
        &openai_api_key,
    )
    .await
    .map_err(|e| format!("Transcription failed: {}", e))?;

    // Clean up the audio file after transcription
    if let Err(err) = std::fs::remove_file(&file_path) {
        log::warn!("Failed to delete audio file {}: {}", file_path, err);
    } else {
        log::info!("Successfully deleted audio file: {}", file_path);
    }

    Ok(transcription)
}

// Whisper model management commands
fn get_whisper_model_id(app_handle: &AppHandle) -> String {
    app_handle.db(|db| {
        get_setting(db, "whisper_model")
            .map(|s| s.setting_value)
            .unwrap_or_default()
    })
}

#[tauri::command]
fn check_whisper_model(app_handle: AppHandle) -> bool {
    let model_id = get_whisper_model_id(&app_handle);
    crate::engine::whisper_engine::is_model_downloaded(&model_id)
}

#[tauri::command]
async fn download_whisper_model(app_handle: AppHandle) -> Result<(), String> {
    let model_id = get_whisper_model_id(&app_handle);
    crate::engine::whisper_engine::download_model(&app_handle, &model_id)
        .await
        .map_err(|e| format!("{}", e))
}

#[tauri::command]
async fn init_whisper_model(app_handle: AppHandle) -> Result<(), String> {
    let model_id = get_whisper_model_id(&app_handle);
    let engine = tokio::task::spawn_blocking(move || {
        crate::engine::whisper_engine::WhisperEngine::load(&model_id)
    })
    .await
    .map_err(|e| format!("Join error: {}", e))?
    .map_err(|e| format!("{}", e))?;

    let mut guard = WHISPER_ENGINE.lock().unwrap();
    *guard = Some(engine);
    info!("Whisper engine initialized");
    Ok(())
}

#[tauri::command]
fn get_transcript() -> String {
    let t = ACCUMULATED_TRANSCRIPT.lock().unwrap();
    t.clone()
}

/// Process a chunk of raw audio: resample → transcribe with Whisper.
/// No RMS gate — Whisper itself handles silence (returns empty), so we let
/// every chunk through to avoid dropping quiet speech (soft speakers, laptop
/// speaker playback, distant voices).
/// Transcribe a buffer directly, for the self-test. Bypasses the silence gate
/// so the report can say "heard nothing" instead of silently skipping.
pub(crate) fn transcribe_samples_for_test(raw_samples: &[f32]) -> Option<String> {
    use crate::engine::audio_processor::resample;

    let device_rate = crate::engine::audio_engine::DEVICE_SAMPLE_RATE
        .load(std::sync::atomic::Ordering::SeqCst);
    if device_rate == 0 || raw_samples.is_empty() {
        return None;
    }
    let samples_16k = resample(raw_samples, device_rate, 16000).ok()?;

    let guard = WHISPER_ENGINE.lock().unwrap();
    let engine = guard.as_ref()?;
    match engine.transcribe(&samples_16k) {
        Ok(text) if !text.trim().is_empty() => Some(text),
        Ok(_) => None,
        Err(err) => {
            log::warn!("Self-test transcription error: {}", err);
            None
        }
    }
}

/// Below this RMS a chunk carries no speech worth transcribing.
///
/// This matters for more than saving cycles: given silence, whisper.cpp
/// reliably invents something - "Thank you.", "Dziękuję.", subtitle credits -
/// because it always decodes *something*. A recorder that runs through a
/// forty-minute meeting where one mostly listens would fill the transcript
/// with those, and the extraction pass downstream would treat them as things
/// that were said.
const SILENCE_RMS: f32 = 0.004;

/// Transcribe a chunk and keep whisper's segment timings, offset so they are
/// relative to the whole recording rather than to the chunk.
fn process_and_transcribe_segments(
    raw_samples: &[f32],
    chunk_start_ms: u64,
) -> Vec<engine::whisper_engine::Segment> {
    use crate::engine::audio_processor::resample;

    let device_rate = crate::engine::audio_engine::DEVICE_SAMPLE_RATE
        .load(std::sync::atomic::Ordering::SeqCst);
    if device_rate == 0 || raw_samples.is_empty() {
        return Vec::new();
    }

    let rms = (raw_samples.iter().map(|s| s * s).sum::<f32>() / raw_samples.len() as f32).sqrt();
    if rms < SILENCE_RMS {
        return Vec::new();
    }

    let samples_16k = match resample(raw_samples, device_rate, 16000) {
        Ok(samples) => samples,
        Err(err) => {
            log::warn!("Resample to 16kHz failed: {}", err);
            return Vec::new();
        }
    };

    let guard = WHISPER_ENGINE.lock().unwrap();
    let Some(engine) = guard.as_ref() else {
        log::warn!("Whisper engine not initialized");
        return Vec::new();
    };

    match engine.transcribe_segments(&samples_16k) {
        Ok(mut segments) => {
            for segment in &mut segments {
                segment.start_ms += chunk_start_ms;
                segment.end_ms += chunk_start_ms;
            }
            segments
        }
        Err(err) => {
            log::warn!("Whisper transcription error: {}", err);
            Vec::new()
        }
    }
}

fn process_and_transcribe_chunk(raw_samples: &[f32]) -> Option<String> {
    use crate::engine::audio_processor::resample;

    let device_rate = crate::engine::audio_engine::DEVICE_SAMPLE_RATE
        .load(std::sync::atomic::Ordering::SeqCst);
    if device_rate == 0 {
        return None;
    }

    if raw_samples.is_empty() {
        return None;
    }
    let rms = (raw_samples.iter().map(|s| s * s).sum::<f32>() / raw_samples.len() as f32).sqrt();
    if rms < SILENCE_RMS {
        return None;
    }

    // Resample directly to 16kHz for Whisper. Whisper-large-v3-turbo is robust
    // to noise on its own, and RNNoise was crushing speech amplitude.
    let samples_16k = match resample(raw_samples, device_rate, 16000) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("Resample to 16kHz failed: {}", e);
            return None;
        }
    };

    // Transcribe
    let guard = WHISPER_ENGINE.lock().unwrap();
    if let Some(engine) = guard.as_ref() {
        match engine.transcribe(&samples_16k) {
            Ok(text) if !text.is_empty() => Some(text),
            Ok(_) => None,
            Err(e) => {
                log::warn!("Whisper transcription error: {}", e);
                None
            }
        }
    } else {
        log::warn!("Whisper engine not initialized");
        None
    }
}

/// Transcribe the two sides separately instead of mixing them. Defaults to on
/// in CUDA builds, where the extra pass is cheap.
static SEPARATE_SPEAKERS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(cfg!(feature = "cuda"));

fn others_label() -> String {
    OTHERS_LABEL.lock().unwrap().clone()
}

/// Format a position in the recording as [mm:ss], so both a reader and a model
/// can point at a moment.
fn timestamp_label(position_ms: u64) -> String {
    let total_seconds = position_ms / 1000;
    format!("[{:02}:{:02}]", total_seconds / 60, total_seconds % 60)
}

/// Append text under a speaker heading, starting a new paragraph - with a
/// timestamp - only when the speaker changes.
fn append_labelled(transcript: &mut String, label: &str, position_ms: u64, text: &str) {
    let mut last = LAST_SPEAKER.lock().unwrap();
    if last.as_deref() == Some(label) {
        if !transcript.is_empty() {
            transcript.push(' ');
        }
    } else {
        if !transcript.is_empty() {
            transcript.push_str("\n\n");
        }
        transcript.push_str(&format!("{} **{}:** ", timestamp_label(position_ms), label));
        *last = Some(label.to_string());
    }
    transcript.push_str(text);
}

/// Append text with no speaker attribution - used when system audio is off and
/// there is nothing to compare the microphone against.
fn append_plain(transcript: &mut String, text: &str) {
    if !transcript.is_empty() {
        transcript.push(' ');
    }
    transcript.push_str(text);
}

/// Append the last scrap of audio transcribed after a recording stops.
fn append_tail(transcript: &mut String, position_ms: u64, text: &str) {
    match speaker_label_for_chunk() {
        Some(label) => append_labelled(transcript, &label, position_ms, text),
        None => append_plain(transcript, text),
    }
}

/// Which side was louder over the chunk just processed, as a label.
fn speaker_label_for_chunk() -> Option<String> {
    use crate::engine::audio_engine::SpeakerHint;

    let others = others_label();
    match crate::engine::audio_engine::take_speaker_hint() {
        SpeakerHint::Me => Some("Me".to_string()),
        SpeakerHint::Others => Some(others),
        SpeakerHint::Both => Some(format!("Me + {}", others.to_lowercase())),
        SpeakerHint::Unknown => None,
    }
}

/// Note a transcribed stretch of speech with its place in the recording.
///
/// Consecutive pieces from the same speaker are merged when the break looks
/// like an artefact of chunking rather than a pause: audio is cut every few
/// seconds regardless of where sentences fall, and a sentence split across two
/// chunks would otherwise arrive as two utterances broken mid-clause.
fn record_utterance(start_ms: u64, end_ms: u64, speaker: &str, text: &str) {
    if !AUTO_RECORDING.load(std::sync::atomic::Ordering::SeqCst) {
        return;
    }

    let text = text.trim();
    if text.is_empty() {
        return;
    }

    let mut utterances = AUTO_UTTERANCES.lock().unwrap();

    if let Some(previous) = utterances.last_mut() {
        let same_speaker = previous.speaker == speaker;
        let gap_ms = start_ms.saturating_sub(previous.end_ms);
        let unfinished = !previous
            .text
            .trim_end()
            .ends_with(['.', '!', '?', '…', ':']);

        if same_speaker && gap_ms < 400 && unfinished {
            previous.text.push(' ');
            previous.text.push_str(text);
            previous.end_ms = end_ms.max(previous.end_ms);
            return;
        }
    }

    utterances.push(engine::transcript_export::Utterance {
        start_ms,
        end_ms,
        speaker: speaker.to_string(),
        text: text.to_string(),
    });
}

/// Realtime transcription loop — polls the audio buffer every 50ms,
/// accumulates ~2s chunks, transcribes, and emits events
async fn realtime_transcription_loop(app_handle: AppHandle) {
    use crate::engine::audio_engine::{
        take_new_samples, take_new_samples_split, DEVICE_SAMPLE_RATE, IS_RECORDING,
    };

    // Transcribing each side separately doubles the work, which is affordable
    // on a GPU and usually is not on a laptop CPU - so it follows the build
    // flavour unless the setting says otherwise.
    let separate = SEPARATE_SPEAKERS.load(std::sync::atomic::Ordering::SeqCst);

    let mut pending: Vec<f32> = Vec::new();
    let mut pending_system: Vec<f32> = Vec::new();
    let mut silence_count: u32 = 0;
    // Samples consumed so far, which is the recording's own clock - steadier
    // than wall time, since it cannot drift when transcription lags.
    let mut consumed_samples: usize = 0;

    loop {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        if !IS_RECORDING.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }

        let new = if separate {
            let (microphone, system) = take_new_samples_split();
            pending_system.extend_from_slice(&system);
            microphone
        } else {
            take_new_samples()
        };

        if new.is_empty() {
            continue;
        }
        pending.extend_from_slice(&new);

        let device_rate = DEVICE_SAMPLE_RATE.load(std::sync::atomic::Ordering::SeqCst);
        if device_rate == 0 {
            continue;
        }

        let chunk_duration_samples = (device_rate as usize) * 3; // 3 seconds
        let min_chunk_samples = (device_rate as usize) * 2;    // 2 seconds minimum

        // Check if we have enough for a chunk, or if there's a silence gap
        let rms: f32 = if new.len() > 0 {
            (new.iter().map(|s| s * s).sum::<f32>() / new.len() as f32).sqrt()
        } else {
            0.0
        };

        if rms < 0.005 {
            silence_count += 1;
        } else {
            silence_count = 0;
        }

        let should_process = pending.len() >= chunk_duration_samples
            || (silence_count >= 10 && pending.len() >= min_chunk_samples);

        if !should_process {
            continue;
        }

        let chunk: Vec<f32> = pending.drain(..).collect();
        let chunk_system: Vec<f32> = pending_system.drain(..).collect();
        silence_count = 0;

        let start_ms = (consumed_samples as u64) * 1000 / device_rate as u64;
        consumed_samples += chunk.len();
        let end_ms = (consumed_samples as u64) * 1000 / device_rate as u64;

        // Transcribe on a blocking thread to avoid blocking the async runtime
        let app = app_handle.clone();
        let transcript_arc = ACCUMULATED_TRANSCRIPT.clone();
        tokio::task::spawn_blocking(move || {
            let mut updated = false;

            if separate {
                // Each side is transcribed on its own, so a label is a fact
                // about which stream carried the speech, not a guess from
                // loudness. A silent side transcribes to nothing and is
                // skipped.
                let others = others_label();
                for (label, samples) in [("Me", &chunk), (others.as_str(), &chunk_system)] {
                    if samples.is_empty() {
                        continue;
                    }
                    for segment in process_and_transcribe_segments(samples, start_ms) {
                        let mut t = transcript_arc.lock().unwrap();
                        append_labelled(&mut t, label, segment.start_ms, &segment.text);
                        record_utterance(segment.start_ms, segment.end_ms, label, &segment.text);
                        updated = true;
                    }
                }
            } else {
                let segments = process_and_transcribe_segments(&chunk, start_ms);
                if !segments.is_empty() {
                    // One reading of the loudness balance for the whole chunk:
                    // the sides were measured over that window, not per
                    // sentence.
                    let label = speaker_label_for_chunk();
                    for segment in segments {
                        let mut t = transcript_arc.lock().unwrap();
                        match &label {
                            Some(label) => {
                                append_labelled(&mut t, label, segment.start_ms, &segment.text);
                                record_utterance(
                                    segment.start_ms,
                                    segment.end_ms,
                                    label,
                                    &segment.text,
                                );
                            }
                            None => {
                                append_plain(&mut t, &segment.text);
                                record_utterance(
                                    segment.start_ms,
                                    segment.end_ms,
                                    "Unknown",
                                    &segment.text,
                                );
                            }
                        }
                        updated = true;
                    }
                }
            }

            if updated {
                let current = transcript_arc.lock().unwrap().clone();

                // Keep the meeting on disk as it goes, so an interrupted
                // recording still leaves what it had.
                if AUTO_RECORDING.load(std::sync::atomic::Ordering::SeqCst) {
                    let meta = AUTO_RECORD_META.lock().unwrap().clone();
                    if let Some((source_app, started_at)) = meta {
                        engine::transcript_export::write_partial(
                            &app,
                            &source_app,
                            &current,
                            started_at,
                        );
                    }
                }

                if let Some(w) = app.get_window("main") {
                    let _ = w.emit("transcript-update", serde_json::json!({
                        "text": current,
                        "is_final": false
                    }));
                }
            }
        });
    }
}

// Document import commands
#[tauri::command]
async fn extract_document_text(file_path: String) -> Result<String, String> {
    use std::path::Path;
    
    log::info!("Extracting text from document: {}", file_path);
    
    // Check if file exists
    if !Path::new(&file_path).exists() {
        return Err(format!("File not found: {}", file_path));
    }
    
    // Determine file type based on extension
    let path = Path::new(&file_path);
    let extension = path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_lowercase())
        .unwrap_or_default();
    
    log::info!("File extension detected: {}", extension);
    
    match extension.as_str() {
        "pdf" => {
            log::info!("Attempting to extract text from PDF...");
            extract_text_from_pdf(&file_path)
        },
        "txt" | "md" | "rtf" => {
            log::info!("Reading text file...");
            read_text_file(&file_path)
        },
        "docx" => {
            log::info!("Attempting to extract text from DOCX...");
            extract_text_from_docx(&file_path)
        },
        _ => Err(format!("Unsupported file format: {}. Supported formats: PDF, TXT, MD, RTF, DOCX", extension))
    }
}

fn extract_text_from_pdf(file_path: &str) -> Result<String, String> {
    match pdf_extract::extract_text(file_path) {
        Ok(text) => {
            log::info!("Successfully extracted {} characters from PDF", text.len());
            if text.trim().is_empty() {
                Err("PDF appears to be empty or contains only images/non-text content".to_string())
            } else {
                Ok(text)
            }
        },
        Err(err) => {
            log::error!("PDF extraction error: {:?}", err);
            Err(format!("Failed to extract text from PDF: {}. Make sure the PDF contains text (not just images).", err))
        }
    }
}

fn extract_text_from_docx(file_path: &str) -> Result<String, String> {
    // Read the file bytes
    let bytes = std::fs::read(file_path).map_err(|e| format!("Failed to read DOCX file: {}", e))?;
    
    log::info!("DOCX file size: {} bytes", bytes.len());
    
    // DOCX files are ZIP archives containing XML
    // We'll extract text from the document.xml inside
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| format!("Failed to open DOCX archive: {}", e))?;
    
    // Find and read word/document.xml
    let mut doc_xml = archive.by_name("word/document.xml")
        .map_err(|_| "DOCX file does not contain document.xml")?;
    
    let mut xml_content = String::new();
    std::io::Read::read_to_string(&mut doc_xml, &mut xml_content)
        .map_err(|e| format!("Failed to read document.xml: {}", e))?;
    
    // Extract text between <w:t> tags (Word text elements)
    let mut extracted_text = String::new();
    let mut in_text_element = false;
    let mut current_text = String::new();
    let mut tag_buffer = String::new();
    let mut in_tag = false;
    
    for c in xml_content.chars() {
        if c == '<' {
            in_tag = true;
            tag_buffer.clear();
            if !current_text.is_empty() && in_text_element {
                extracted_text.push_str(&current_text);
                current_text.clear();
            }
        } else if c == '>' {
            in_tag = false;
            // Check if it's a text element opening or closing
            if tag_buffer.starts_with("w:t") && !tag_buffer.starts_with("w:t ") || tag_buffer.starts_with("w:t ") {
                in_text_element = true;
            } else if tag_buffer == "/w:t" {
                in_text_element = false;
            } else if tag_buffer == "/w:p" {
                // End of paragraph - add newline
                extracted_text.push('\n');
            }
        } else if in_tag {
            tag_buffer.push(c);
        } else if in_text_element {
            current_text.push(c);
        }
    }
    
    let trimmed = extracted_text.trim().to_string();
    if trimmed.is_empty() {
        Err("DOCX file appears to be empty or could not be parsed".to_string())
    } else {
        log::info!("Successfully extracted {} characters from DOCX", trimmed.len());
        Ok(trimmed)
    }
}

fn read_text_file(file_path: &str) -> Result<String, String> {
    match std::fs::read_to_string(file_path) {
        Ok(content) => {
            log::info!("Successfully read {} characters from text file", content.len());
            Ok(content)
        },
        Err(e) => {
            log::error!("Error reading text file: {:?}", e);
            Err(format!("Failed to read text file: {}", e))
        }
    }
}
