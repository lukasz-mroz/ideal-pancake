# Windows portable build

A portable build is a folder you unzip and run. No installer, no registry
entries, nothing written to `%APPDATA%` — the database, vector indices,
Whisper models, recordings and WebView2 storage all live in a `data` folder
next to `Platypus.exe`, so the app can travel on a USB stick.

## Getting a build

**From GitHub Actions (recommended — nothing to install locally)**

1. Actions → *Windows portable build* → **Run workflow**.
2. Pick the Whisper model to bundle (`large-v3-turbo` ≈ 1.6 GB is the sweet
   spot; `none` keeps the zip ~40 MB and downloads the model on first use).
3. When the run finishes, download the `platypus-portable-windows-x64`
   artifact.

Pushing a tag like `v0.1.0` runs the same workflow and attaches the zip to the
release. Note GitHub's 2 GB limit per release asset — bundling `large-v3`
(3.1 GB) works as an artifact but is too large for a release asset.

**Locally on a Windows machine**

Requirements: Node 18+, Rust, cmake, LLVM and the MSVC C++ build tools -
`scripts\setup-windows-toolchain.cmd` installs all of them with winget (add
`winget install Nvidia.CUDA` for the GPU build). Then:

```powershell
npm install
npm run build
cargo build --release --features "custom-protocol portable" --manifest-path src-tauri\Cargo.toml
powershell -ExecutionPolicy Bypass -File scripts\package-portable.ps1
```

The result lands in `dist-portable\`. Useful switches:

```powershell
# slim package, no model and no WebView2 installer
scripts\package-portable.ps1 -WhisperModel none -IncludeWebView2:$false
# folder only, no zip
scripts\package-portable.ps1 -NoZip
```

## GPU transcription (NVIDIA / CUDA)

Local Whisper runs on the CPU on Windows. Building with the `cuda` feature
moves it onto an NVIDIA GPU instead:

```powershell
scripts\build-portable-cuda.cmd
```

That needs the CUDA Toolkit (`winget install Nvidia.CUDA`) on the build
machine; the packaged folder carries the CUDA runtime DLLs, so the machine
that *runs* it needs only an NVIDIA driver. The output goes to
`dist-portable-cuda\`, leaving any CPU build in `dist-portable\` untouched so
the two can be compared on the same recording.

The *Windows portable build (CUDA)* workflow does the same thing in CI, for
when you would rather not install the toolkit locally. The runner has no GPU,
so it only proves the build compiles.

On the machine that runs the CUDA build, the DLLs can also be fetched
separately instead of riding inside the package:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\fetch-cuda-runtime.ps1 -Destination <portable folder>
```

It pulls cudart and cuBLAS from NVIDIA's redistributable packages (published
on PyPI as plain zip archives - no Python needed) and drops the DLLs next to
`Platypus.exe`. This does not turn a CPU build into a GPU build: the CUDA
backend is compiled into the executable.

VRAM is the practical limit: `large-v3` in fp16 needs about 3.1 GB, which is
tight on a 4 GB card that is also driving the desktop. `large-v3-turbo`
(~1.6 GB) is the safe default there.

## Meeting capture

With the app running (tray icon, window optional) it records meetings on its
own:

1. Detection polls for Teams and Zoom. On Windows "in a meeting" means the app
   currently holds the microphone, read from the same registry data behind
   Windows' own microphone indicator
   (`CapabilityAccessManager\ConsentStore\microphone`, where
   `LastUsedTimeStop = 0` means in use right now).
2. When a meeting starts, recording begins without a prompt and transcription
   runs locally through Whisper.
3. When the meeting ends, the transcript is written to `data\transcripts\` as
   `2026-09-13_15-42_microsoft-teams.md`, with source, times, duration and word
   count in the header - so another tool can read meetings off the filesystem
   without touching the database.

### Both sides of the call

Recording only the microphone captures your own voice and nothing else, so on
Windows the app also captures what the speakers play (WASAPI loopback) and
mixes it into the same stream before transcription. `cpal` has no loopback
mode, so that part talks to WASAPI directly; see
`src-tauri/src/engine/loopback_capture.rs`.

Set `capture_system_audio` to `false` to record the microphone only. The value
is read at startup, so a change takes effect on the next run. On macOS the
setting does nothing - the system has no loopback endpoint to open, and
capturing output there needs a virtual audio device.

### Language

Transcription used to be hard-wired to English, which turns any other language
into phonetic nonsense. The `transcription_language` setting now drives it:
a code (`pl`, `en`, `de`) forces one language, `auto` detects.

`auto` detects **once per recording**, not per chunk. Chunks here are about
three seconds - too short to decide reliably - so per-chunk detection lets one
meeting come back as a mix of languages. Instead the first chunk carrying real
speech decides, and that language holds until the recording stops.

For meetings that are in one language but sprinkled with borrowed terms
(Polish with English jargon, say), forcing the base language beats `auto`: the
model still writes the foreign words in Latin script, and you avoid a
mid-meeting switch. Use `auto` when whole meetings differ in language.

Inference threads follow the machine instead of a hard-coded four.

### What a captured meeting produces

- `data\transcripts\<date>_<app>.md` - the file other tools read
- a note in the Unassigned project, so the app's own search and chat can see
  the meeting too

On first run the app seeds the settings this depends on:
`use_local_transcription`, `whisper_model`, `meeting_detection_enabled`,
`auto_capture_meetings`, `capture_system_audio`, `transcription_language`,
`vectorization_enabled` and `rag_top_k`. Settings you
have already chosen are never overwritten.

The app does not add itself to Windows startup; put a shortcut in the Startup
folder yourself if you want it running all the time. It takes `--minimized` to
start hidden in the tray.

To be asked before recording, set `auto_capture_meetings` to `false` - the
popup-and-banner path returns. `meeting_detection_enabled: false` stops the
detection thread altogether. A recording started by hand is never stopped by a
meeting ending: the automatic path only stops what it started.

Screenshot/task-mining scaffolding stays off unless `task_mining_enabled` is
`true`; it creates directories and a cleanup pass for a feature this build does
not use.

Everyone on the call is a participant in this recording. Whether they are told
is your call, not the software's.

## Updating an installation

Every package carries `VERSION.txt` (version, git commit, flavour, build date),
so it is possible to tell what a machine is running. To update it:

```powershell
git pull
scripts\build-portable.cmd
powershell -ExecutionPolicy Bypass -File scripts\update-portable.ps1 -Target D:\Platypus
```

`update-portable.ps1` replaces everything except `data\` - the database,
transcripts, models and settings stay as they are. It refuses to write while
Platypus is running; `-StopRunning` closes it first, which must not be done
while a meeting is being recorded, since the recording only reaches disk when
the meeting ends.

A bundled Whisper model is copied in only when the installation has none, so an
update never re-downloads or overwrites gigabytes that are already there.

## Helper scripts inside the package

Every package carries two small scripts, run from the unpacked folder:

- `fetch-whisper-model.ps1` - downloads a Whisper model into `data\models`,
  with resume support. Useful when the package was built with
  `whisper_model: none`, which keeps the download off the build and onto the
  machine that will actually transcribe.
- `fetch-cuda-runtime.ps1` - pulls the CUDA runtime DLLs for a GPU build that
  was packaged without them.

## What the package contains

```
Platypus-Portable-<version>-win-x64\
  Platypus.exe
  Start Platypus.cmd     installs WebView2 if missing, then starts the app
  portable.txt           marker file that switches portable mode on
  README.txt
  data\models\           bundled Whisper model
  vendor\                WebView2 runtime installer
```

## How portable mode is decided

`src-tauri/src/configuration/portable.rs` turns it on when any of these holds:

- the binary was compiled with the `portable` cargo feature (what the workflow
  does), or
- `portable.txt` sits next to the executable, or
- `PLATYPUS_PORTABLE` is `1`/`true`/`yes`/`on`.

`PLATYPUS_PORTABLE=0` forces it off. If the folder next to the executable is
not writable — an install under `C:\Program Files`, a read-only drive — the app
silently falls back to the normal `%APPDATA%` paths instead of failing to
start.

## Notes

- WebView2 runtime is a hard requirement of Tauri. Windows 11 and up-to-date
  Windows 10 already have it; the launcher installs it otherwise (per-user, no
  admin rights needed).
- Local Whisper transcription runs on CPU on Windows (Metal acceleration is
  macOS-only), so expect it to be slower than on a Mac.
- Chat still needs whichever LLM you connect. A local Ollama model keeps the
  whole app offline.
