//! Windows-only capture of what the speakers are playing (WASAPI loopback).
//!
//! Recording the microphone alone gives half a meeting: your own voice without
//! the answers. This module captures the render endpoint in loopback mode and
//! hands the samples to the audio engine, which mixes them with the microphone
//! before transcription.
//!
//! `cpal` has no loopback mode, so this talks to WASAPI directly through the
//! `windows` crate. Everything here is a no-op on other platforms.

#[cfg(target_os = "windows")]
mod imp {
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::Mutex;

    use lazy_static::lazy_static;
    use log::{info, warn};

    pub static IS_CAPTURING: AtomicBool = AtomicBool::new(false);
    /// Sample rate the render endpoint runs at, published for the resampler.
    pub static CAPTURE_SAMPLE_RATE: AtomicU32 = AtomicU32::new(0);

    lazy_static! {
        /// Mono samples captured from the speakers, waiting to be mixed in.
        static ref BUFFER: Mutex<Vec<f32>> = Mutex::new(Vec::new());
    }

    pub fn start() {
        if IS_CAPTURING.swap(true, Ordering::SeqCst) {
            return;
        }
        {
            let mut buffer = BUFFER.lock().unwrap();
            buffer.clear();
        }

        std::thread::spawn(|| {
            if let Err(err) = capture_loop() {
                warn!("System audio capture stopped: {}", err);
            }
            IS_CAPTURING.store(false, Ordering::SeqCst);
        });
    }

    pub fn stop() {
        IS_CAPTURING.store(false, Ordering::SeqCst);
    }

    /// Take up to `max` samples, resampled to `target_rate`.
    pub fn take_samples(target_rate: u32, max: usize) -> Vec<f32> {
        let source_rate = CAPTURE_SAMPLE_RATE.load(Ordering::SeqCst);
        if source_rate == 0 || target_rate == 0 || max == 0 {
            return Vec::new();
        }

        // How many source samples correspond to `max` output samples.
        let wanted_source =
            ((max as f64) * (source_rate as f64) / (target_rate as f64)).ceil() as usize;

        let taken: Vec<f32> = {
            let mut buffer = BUFFER.lock().unwrap();
            let take = wanted_source.min(buffer.len());
            buffer.drain(..take).collect()
        };

        if taken.is_empty() {
            return Vec::new();
        }
        if source_rate == target_rate {
            return taken;
        }

        // Linear interpolation is plenty for 16 kHz speech recognition, and
        // avoids carrying resampler state across calls.
        let ratio = source_rate as f64 / target_rate as f64;
        let out_len = ((taken.len() as f64) / ratio).floor() as usize;
        let mut out = Vec::with_capacity(out_len);
        for i in 0..out_len {
            let pos = (i as f64) * ratio;
            let left = pos.floor() as usize;
            let right = (left + 1).min(taken.len() - 1);
            let frac = (pos - left as f64) as f32;
            out.push(taken[left] * (1.0 - frac) + taken[right] * frac);
        }
        out
    }

    fn capture_loop() -> Result<(), String> {
        use windows::core::Interface;
        use windows::Win32::Media::Audio::{
            eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator,
            MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
        };
        use windows::Win32::System::Com::{
            CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
        };

        unsafe {
            // Returns an HRESULT rather than a Result in this version of the
            // windows crate; `.ok()` turns it into one.
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .map_err(|e| format!("CoInitializeEx failed: {}", e))?;

            let result = (|| -> Result<(), String> {
                let enumerator: IMMDeviceEnumerator =
                    CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                        .map_err(|e| format!("No device enumerator: {}", e))?;

                let device = enumerator
                    .GetDefaultAudioEndpoint(eRender, eConsole)
                    .map_err(|e| format!("No default playback device: {}", e))?;

                let audio_client: IAudioClient = device
                    .Activate(CLSCTX_ALL, None)
                    .map_err(|e| format!("Could not activate audio client: {}", e))?;

                let format_ptr = audio_client
                    .GetMixFormat()
                    .map_err(|e| format!("GetMixFormat failed: {}", e))?;
                let format = *format_ptr;

                let channels = format.nChannels as usize;
                let sample_rate = format.nSamplesPerSec;
                let bits = format.wBitsPerSample;
                if channels == 0 {
                    return Err("Playback device reports zero channels".to_string());
                }
                CAPTURE_SAMPLE_RATE.store(sample_rate, Ordering::SeqCst);
                info!(
                    "System audio capture: {} Hz, {} channels, {} bits",
                    sample_rate, channels, bits
                );

                // 1 second buffer, expressed in 100-nanosecond units.
                audio_client
                    .Initialize(
                        AUDCLNT_SHAREMODE_SHARED,
                        AUDCLNT_STREAMFLAGS_LOOPBACK,
                        10_000_000,
                        0,
                        format_ptr,
                        None,
                    )
                    .map_err(|e| format!("Could not initialise loopback capture: {}", e))?;

                let capture_client: IAudioCaptureClient = audio_client
                    .GetService()
                    .map_err(|e| format!("No capture client: {}", e))?;

                audio_client
                    .Start()
                    .map_err(|e| format!("Could not start capture: {}", e))?;

                while IS_CAPTURING.load(Ordering::SeqCst) {
                    let packet = capture_client.GetNextPacketSize().unwrap_or(0);
                    if packet == 0 {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        continue;
                    }

                    let mut data: *mut u8 = std::ptr::null_mut();
                    let mut frames: u32 = 0;
                    let mut flags: u32 = 0;
                    if capture_client
                        .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                        .is_err()
                    {
                        continue;
                    }

                    if frames > 0 {
                        let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                        let mut mono = Vec::with_capacity(frames as usize);

                        if silent || data.is_null() {
                            mono.resize(frames as usize, 0.0);
                        } else if bits == 32 {
                            let samples = std::slice::from_raw_parts(
                                data as *const f32,
                                frames as usize * channels,
                            );
                            for frame in samples.chunks(channels) {
                                mono.push(frame.iter().sum::<f32>() / channels as f32);
                            }
                        } else if bits == 16 {
                            let samples = std::slice::from_raw_parts(
                                data as *const i16,
                                frames as usize * channels,
                            );
                            for frame in samples.chunks(channels) {
                                let sum: f32 =
                                    frame.iter().map(|s| *s as f32 / i16::MAX as f32).sum();
                                mono.push(sum / channels as f32);
                            }
                        } else {
                            mono.resize(frames as usize, 0.0);
                        }

                        let mut buffer = BUFFER.lock().unwrap();
                        // Never let a stalled consumer grow this without bound:
                        // 60 seconds of audio is far more than the mixer needs.
                        let cap = (sample_rate as usize) * 60;
                        if buffer.len() + mono.len() > cap {
                            let overflow = buffer.len() + mono.len() - cap;
                            let trim = overflow.min(buffer.len());
                            buffer.drain(..trim);
                        }
                        buffer.extend_from_slice(&mono);
                    }

                    let _ = capture_client.ReleaseBuffer(frames);
                }

                let _ = audio_client.Stop();
                Ok(())
            })();

            CoUninitialize();
            result
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod imp {
    pub fn start() {}
    pub fn stop() {}
    pub fn take_samples(_target_rate: u32, _max: usize) -> Vec<f32> {
        Vec::new()
    }
}

pub use imp::{start, stop, take_samples};
