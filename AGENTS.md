# Agent runbook: build, install and update the portable Windows package

Instructions for an AI agent working on a Windows machine that runs Platypus as
an unattended meeting recorder. This file is the source of truth for that
machine; the repository changes over time, so **re-read it after every pull**.

Two situations are covered:

- **Updating** an installation that already exists — the common case.
- **First install** on a machine that has nothing yet.

Run all commands from the repository root unless stated otherwise. Windows
10/11 x64 is assumed.

---

## A. Updating to a newer version

The application is under active development: expect new builds regularly. An
update replaces the program and keeps everything the machine has collected.

### The one rule

**Never delete or overwrite the `data\` folder.** It holds the notes database,
the meeting transcripts, the downloaded Whisper models (gigabytes) and the
settings. The program files around it are disposable; `data\` is not. There is
no backup anywhere else.

### Steps

1. **Check what is installed.** Every package carries `VERSION.txt`:

   ```
   type <install folder>\VERSION.txt
   ```

   It names the version, the git commit it was built from, the flavour (`cpu`
   or `cuda`) and the build date.

2. **Fetch the new code.**

   ```
   git pull
   ```

   Compare `git rev-parse --short HEAD` with the `commit=` line from step 1. If
   they match, the installation is already current — stop here, do not rebuild.

3. **Re-read this file and `docs\WINDOWS-PORTABLE.md`.** Both travel with the
   code; a new version may change how the build works or add a setting.

4. **Build the same flavour that is installed** (the `flavour=` line):

   ```
   scripts\build-portable.cmd        (cpu)
   scripts\build-portable-cuda.cmd   (cuda)
   ```

   A rebuild after a pull takes minutes; a cold cargo cache takes 15-40.

5. **Install over the old one**, keeping `data\`:

   ```
   powershell -ExecutionPolicy Bypass -File scripts\update-portable.ps1 -Target <install folder>
   ```

   The script copies everything except `data\`, adds a bundled model only when
   none is installed, and refuses to run while Platypus is open. Add
   `-StopRunning` to close it first — but **never while a meeting is being
   recorded**: the recording lives in memory until the meeting ends, and
   killing the process throws it away.

6. **Verify** the new `VERSION.txt` in the install folder shows the commit from
   step 2, then start the app and confirm the tray icon appears.

### What an update must not do

- Do not delete `data\`, any file in it, or the install folder itself.
- Do not "clean up" old transcripts — they are the product of this machine.
- Do not change settings in `data\platypus.sqlite` unless asked; the app seeds
  what it needs and respects choices already made.
- Do not reinstall the toolchain when a build fails. Read the log first.

---

## B. First install

### 1. Establish what to build

| Flavour | Script | Needs | Output |
| --- | --- | --- | --- |
| CPU | `scripts\build-portable.cmd` | Node, Rust, cmake, LLVM, MSVC C++ | `dist-portable\` |
| NVIDIA GPU | `scripts\build-portable-cuda.cmd` | the above **plus** CUDA Toolkit | `dist-portable-cuda\` |

```
nvidia-smi --query-gpu=name,memory.total --format=csv,noheader
```

- A card is named → build the GPU flavour.
- Command not found, or no device → build the CPU flavour. A CUDA build links
  the NVIDIA driver library and will not start without a card.

The GPU architecture is pinned in `src-tauri\cuda-toolchain.cmake`; `75` is
Turing (Quadro T600/T1200, GTX 16xx, RTX 20xx), `86` Ampere, `89` Ada. A
mismatch appears at run time as "no kernel image is available for execution on
the device", not at build time.

### 2. Install the toolchain

```
scripts\setup-windows-toolchain.cmd
```

Node LTS, Rustup, cmake, LLVM and the MSVC C++ build tools, through winget. For
the GPU flavour also `winget install --id Nvidia.CUDA`, **after** Visual Studio:
the CUDA installer wires itself into the Visual Studio versions it finds, and
the other order ends in `No CUDA toolset found`.

**Then open a new console.** The installers set `PATH`, `LIBCLANG_PATH` and
`CUDA_PATH`, and a shell already running will not see them. This is the most
common reason the build script reports a missing prerequisite that is installed.

### 3. Verify the toolchain

```
node -v
cargo -V
cmake --version
nvcc --version
dir "%LIBCLANG_PATH%\libclang.dll"
```

Node 18+, any recent cargo and cmake. `nvcc` matters only for the GPU flavour.
If `LIBCLANG_PATH` is empty: `setx LIBCLANG_PATH "C:\Program Files\LLVM\bin"`,
then open another console. `bindgen` needs `libclang.dll` for the whisper.cpp
bindings.

### 4. Build

```
scripts\build-portable.cmd
```

Full output goes to `build-portable.log` (or `build-portable-cuda.log`); on
failure the script prints the last 30 lines. **The first build takes 15-40
minutes** — whisper.cpp, SQLite and the Tauri dependency tree compile from
scratch, and with CUDA nvcc compiles kernels on top. Do not interrupt it and do
not conclude it has hung.

### 5. Install

Copy the folder from `dist-portable\` (or `dist-portable-cuda\`) wherever it
should live. It is self-contained: the executable, its `data\` folder and the
helper scripts.

**Never run the app from `dist-portable\` as the real installation.** That
folder is wiped and rebuilt on every package, taking its `data\` with it -
notes, transcripts and downloaded models included. Packaging also refuses to
run while Platypus is open, because a live instance holds WebView2's files.

Nothing is written to `%APPDATA%` or the registry. The database, vector
indices, Whisper models, recordings, logs and WebView2 storage all sit in
`data\` next to the executable.

### 6. First run

`Start Platypus.cmd` installs the WebView2 runtime if Windows lacks it, then
launches the app. The executable is unsigned, so SmartScreen warns on first
launch.

Fetch a Whisper model up front, with resume support:

```
powershell -ExecutionPolicy Bypass -File fetch-whisper-model.ps1 -Model large-v3-turbo
```

**On a 4 GB card use `large-v3-turbo`**: `large-v3` in fp16 needs ~3.1 GB of
weights and will not fit next to the desktop. The model chosen in Settings must
match the file that is present.

The app does not add itself to Windows startup. If it should run all the time,
put a shortcut with the `--minimized` argument in the Startup folder.

---

## C. What the application does on this machine

- Detects Teams and Zoom meetings by watching which application holds the
  microphone (the registry data behind Windows' own microphone indicator).
- Starts recording on its own, with no prompt, and stops when the meeting ends.
- Records the microphone **and** the speakers (WASAPI loopback), so both sides
  of a call end up in the transcript.
- Transcribes locally with Whisper; the spoken language is detected once per
  recording and held for its duration (`transcription_language` forces a code
  like `pl` instead).
- Labels transcript lines `Me` / `Others`. CUDA builds transcribe each stream
  separately, so the label reflects which stream the speech came from; CPU
  builds mix the streams and label by loudness balance (`Me + others` when both
  talk at once). `separate_speaker_streams` overrides the default. Neither
  names people.
- Writes three things to `data\transcripts\`: the transcript as `<date>_<meeting>.md`
  (named after the Teams window title when there is one), a `.json` companion
  listing utterances with `start_ms`/`end_ms`/`speaker`, and an appended line in
  `index.jsonl`. The meeting is also stored as a note inside the app.
- After the files are written, a model extracts `decisions` (each with
  `changed_from`), `action_items`, `topics` and `open_questions` into the
  `.json` under `analysis`. Provider comes from `post_meeting_analysis`
  (`local` = Ollama, or `claude`/`openai`/`gemini`/`off`).
- While a meeting runs, the transcript is kept in `<time>.partial.md` and
  replaced by the finished file at the end. A leftover `.partial.md` means a
  recording that was interrupted.

Settings live in `data\platypus.sqlite` and are seeded on first run:
`use_local_transcription`, `whisper_model`, `meeting_detection_enabled`,
`auto_capture_meetings`, `capture_system_audio`, `transcription_language`,
`vectorization_enabled`, `rag_top_k`.

---

## D. Known failure modes

| Symptom in the log | Cause and fix |
| --- | --- |
| `Could not resolve host: index.crates.io` / `static.crates.io` | DNS or a domain blocker on this machine. npm working proves nothing — different host. Check a site blocker, security suite or filtering DNS, then rerun. |
| `No CUDA toolset found` | CUDA's MSBuild integration missing for the installed Visual Studio (CUDA installed before VS, or a VS newer than that CUDA). Reinstall CUDA after VS, or use a CUDA release that supports this VS. |
| `unsupported Microsoft Visual Studio version` | nvcc rejects the MSVC host compiler. Newer CUDA Toolkit, or install the VS 2022 build tools alongside. |
| `libclang.dll` not found | `LIBCLANG_PATH` unset or wrong — section B.3. |
| `ERESOLVE could not resolve` during `npm install` | Dependency conflict. The script retries without `package-lock.json`; if it still fails, report the conflicting packages rather than forcing `--legacy-peer-deps`, which hides a real version mismatch. |
| A window flashes and closes with no output | A `.cmd` file with LF line endings. `.gitattributes` forces CRLF on checkout; convert the file if it came from elsewhere. |
| No transcript after a meeting | Check in order: `data\logs` shows `Meeting detected on`; `System audio capture:` appears there too; Windows' microphone indicator showed Teams using the mic during the call. |

---

## E. Working rules

- Build output (`dist-portable*`, `target`, `node_modules`) is git-ignored.
  Never commit it.
- Keep `.cmd` and `.ps1` files CRLF; `.gitattributes` enforces this.
- The *Windows portable build (CUDA)* GitHub workflow is broken on GitHub's
  runner image (its Visual Studio is newer than the CUDA release the workflow
  installs). Build the GPU flavour locally rather than debugging CI, unless
  fixing that workflow is the task you were given.
- When a build fails, report the relevant log lines instead of switching
  approaches. Most failures here are environmental and listed above.
