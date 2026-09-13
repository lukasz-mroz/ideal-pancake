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

Requirements: Node 18+, Rust, cmake, LLVM (see the main README), then:

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
