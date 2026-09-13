//! Checks the capture chain end to end without needing a meeting.
//!
//! An unattended recorder fails quietly: no window, no user, and the first
//! sign of trouble is an empty transcript after a conversation that mattered.
//! This plays a tone through the speakers while recording, transcribes what
//! came back, and writes down what worked - so a freshly installed machine can
//! be verified in half a minute.

use std::time::Duration;

use log::info;

use crate::configuration::portable;
use crate::configuration::state::ServiceAccess;
use crate::engine::{audio_engine, loopback_capture, whisper_engine};

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |acc, s| acc.max(s.abs()))
}

/// Play a quiet 440 Hz tone, so loopback capture has something to hear even in
/// a silent room.
fn play_tone(seconds: u64) -> Result<(), String> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| "No default playback device".to_string())?;
    let config = device
        .default_output_config()
        .map_err(|e| format!("No playback config: {}", e))?;

    let sample_rate = config.sample_rate().0 as f32;
    let channels = config.channels() as usize;
    let mut phase = 0.0f32;

    let stream = device
        .build_output_stream(
            &config.into(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                for frame in data.chunks_mut(channels) {
                    phase += 440.0 * std::f32::consts::TAU / sample_rate;
                    if phase > std::f32::consts::TAU {
                        phase -= std::f32::consts::TAU;
                    }
                    // Audible, but not startling in a quiet office.
                    let value = phase.sin() * 0.2;
                    for sample in frame.iter_mut() {
                        *sample = value;
                    }
                }
            },
            |err| log::warn!("Self-test playback error: {}", err),
            None,
        )
        .map_err(|e| format!("Could not open playback stream: {}", e))?;

    stream.play().map_err(|e| format!("Could not play: {}", e))?;
    std::thread::sleep(Duration::from_secs(seconds));
    drop(stream);
    Ok(())
}

/// Run every check and return the report as Markdown.
pub async fn run(app_handle: &tauri::AppHandle) -> String {
    let mut report = String::new();
    report.push_str("# Platypus self-test\n\n");
    report.push_str(&format!(
        "- Run at: {}\n- Build: {}\n\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
        if cfg!(feature = "cuda") { "CUDA" } else { "CPU" }
    ));

    // --- where things are written -------------------------------------------
    match portable::app_data_dir(app_handle) {
        Some(dir) => {
            let probe = dir.join(".self-test-probe");
            match std::fs::write(&probe, b"x") {
                Ok(()) => {
                    let _ = std::fs::remove_file(&probe);
                    report.push_str(&format!("- [ok] Data directory writable: {}\n", dir.display()));
                }
                Err(err) => report.push_str(&format!(
                    "- [FAIL] Data directory not writable ({}): {}\n",
                    dir.display(),
                    err
                )),
            }
        }
        None => report.push_str("- [FAIL] Could not resolve the data directory\n"),
    }

    // --- the model -----------------------------------------------------------
    let model_id = app_handle
        .db(|db| crate::repository::settings_repository::get_setting(db, "whisper_model"))
        .map(|setting| setting.setting_value)
        .unwrap_or_default();
    let model_id = if model_id.trim().is_empty() {
        "large-v3".to_string()
    } else {
        model_id
    };

    if whisper_engine::is_model_downloaded(&model_id) {
        report.push_str(&format!("- [ok] Whisper model present: {}\n", model_id));
    } else {
        report.push_str(&format!(
            "- [FAIL] Whisper model missing: {} - run fetch-whisper-model.ps1\n",
            model_id
        ));
    }

    // --- capture -------------------------------------------------------------
    report.push_str("\n## Capture\n\n");

    if let Err(err) = audio_engine::start_recording_local().await {
        report.push_str(&format!("- [FAIL] Could not start recording: {}\n", err));
        return report;
    }

    // Discard the first moment: streams take a beat to settle.
    tokio::time::sleep(Duration::from_millis(700)).await;
    let _ = audio_engine::take_new_samples();

    report.push_str("- Playing a 440 Hz tone for 3 seconds - speak now as well\n");
    let tone = tokio::task::spawn_blocking(|| play_tone(3)).await;
    match tone {
        Ok(Ok(())) => report.push_str("- [ok] Playback stream opened\n"),
        Ok(Err(err)) => report.push_str(&format!("- [warn] Could not play the tone: {}\n", err)),
        Err(err) => report.push_str(&format!("- [warn] Playback task failed: {}\n", err)),
    }

    let (microphone, system) = audio_engine::take_new_samples_split();
    let _ = audio_engine::stop_recording_local().await;
    loopback_capture::stop();

    let mic_rms = rms(&microphone);
    let mic_peak = peak(&microphone);
    if microphone.is_empty() {
        report.push_str("- [FAIL] No microphone samples captured\n");
    } else if mic_peak < 0.001 {
        report.push_str(&format!(
            "- [FAIL] Microphone silent ({} samples, peak {:.4}) - check the input device and Windows privacy settings\n",
            microphone.len(),
            mic_peak
        ));
    } else {
        report.push_str(&format!(
            "- [ok] Microphone captured {} samples (rms {:.4}, peak {:.4})\n",
            microphone.len(),
            mic_rms,
            mic_peak
        ));
    }

    let system_peak = peak(&system);
    if system.is_empty() {
        report.push_str(
            "- [FAIL] No system audio captured - loopback did not open (see the log for 'System audio capture:')\n",
        );
    } else if system_peak < 0.001 {
        report.push_str(&format!(
            "- [FAIL] System audio silent ({} samples) - the tone did not reach the capture; is the output device muted?\n",
            system.len()
        ));
    } else {
        report.push_str(&format!(
            "- [ok] System audio captured {} samples (peak {:.4}) - the tone came back\n",
            system.len(),
            system_peak
        ));
    }

    // --- transcription --------------------------------------------------------
    report.push_str("\n## Transcription\n\n");
    if microphone.is_empty() {
        report.push_str("- skipped, nothing was recorded\n");
    } else {
        match tokio::task::spawn_blocking(move || crate::transcribe_samples_for_test(&microphone))
            .await
        {
            Ok(Some(text)) => report.push_str(&format!(
                "- [ok] Whisper returned: \"{}\"\n",
                text.replace('\n', " ")
            )),
            Ok(None) => report.push_str(
                "- [warn] Whisper returned nothing - either silence, or the model is not loaded\n",
            ),
            Err(err) => report.push_str(&format!("- [FAIL] Transcription task failed: {}\n", err)),
        }
    }

    report.push_str("\n## Meeting detection\n\n");
    report.push_str(
        "- Detection watches which application holds the microphone. To check it, join a call and look for 'Meeting detected on' in data/logs.\n",
    );

    info!("Self-test finished");
    report
}
