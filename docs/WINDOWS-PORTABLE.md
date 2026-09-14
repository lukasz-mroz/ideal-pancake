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
   `2026-09-13_15-42_weekly-sync-microsoft-teams.md`, with source, times,
   duration and word count in the header - so another tool can read meetings
   off the filesystem without touching the database. The name comes from the
   Teams window title when it offers one, which is why a transcript is usually
   named after the meeting rather than the app.

### Who said what

Transcript lines are labelled `Me` and `Others`, decided by which side was
louder while that piece of audio was recorded - the microphone or the
speakers. When both are talking the line is marked `Me + others` rather than
guessed.

How those labels are produced depends on the build:

- **CUDA builds** transcribe each stream separately, so a label states which
  stream carried the speech rather than inferring it. A silent side produces
  nothing and is skipped. This costs a second inference pass per chunk, which a
  GPU absorbs.
- **CPU builds** mix the streams and label by loudness balance, because a
  second pass on a laptop CPU would not keep up with the conversation.

`separate_speaker_streams` overrides the default either way.

### Naming the other person

In a one-to-one call Teams titles its window with the other participant, so
that name replaces `Others` in the transcript and appears in the file header.
A scheduled meeting is titled with the meeting's name instead, and nothing in
the title says which of the two it is - so the test is strict: two or three
capitalised words, no digits, and none of the words that turn up in meeting
names (`sync`, `standup`, `spotkanie`, ...). Anything else keeps the neutral
`Others`, because a wrong name is worse than no name.

Neither approach names people in a group call, and neither separates two voices
arriving through the same speaker - both sides of that are `Others`. Labels only appear
when `capture_system_audio` is on; with the microphone alone there is no second
stream.

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

### Do not use the build output as the installation

`dist-portable\` is rebuilt from scratch every time you package, so anything
that accumulates there - notes, transcripts, downloaded models - is deleted on
the next build. Copy the folder somewhere else and run it from there; use
`scripts\update-portable.ps1` to move new builds into it.

Packaging also refuses to run while Platypus is open: a running instance holds
WebView2's memory-mapped files, and the old staging folder cannot be replaced
underneath it.

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

### Checking a machine works

```
Platypus.exe --self-test
```

Records for a few seconds while playing a 440 Hz tone through the speakers,
then reports whether the data directory is writable, the Whisper model is
present, the microphone captured anything, the tone came back through loopback
capture, and what Whisper made of it. The report goes to `data\self-test.md`
and to stdout, and the app exits.

Worth running after every install and after any update on a machine nobody
watches: an unattended recorder fails silently, and the first sign is otherwise
an empty transcript after a conversation that mattered.

### Silence

Chunks below a low energy threshold are never sent to Whisper. This is not
only about saving cycles: given silence, whisper.cpp reliably invents
something - "Thank you.", "Dziękuję.", subtitle credits - because it always
decodes *something*. In a meeting where one mostly listens, those phantom
lines would otherwise fill the transcript and be read as statements by the
extraction pass.

### What a finished meeting leaves behind

```
data\transcripts\
  2026-09-13_15-42_weekly-sync-microsoft-teams.md     prose, for people
  2026-09-13_15-42_weekly-sync-microsoft-teams.json   utterances with timings
  index.jsonl                                          one line per meeting
```

The `.json` companion lists every stretch of speech with `start_ms`, `end_ms`,
`start_iso`, `speaker` and `text`. Timings come from whisper's own segment
boundaries, so they are per sentence rather than per three-second chunk, and
they are measured on the audio clock rather than wall time - they hold even
when transcription lags behind the conversation. `start_iso` gives the same
moment as a wall-clock time, so joining a transcript with anything else from
that day needs no arithmetic.

Pieces split by a chunk boundary are stitched back together: the same speaker,
a gap under 400 ms and a previous line that does not end in sentence
punctuation means one utterance that got cut, not two.

Every file carries a `meeting_id` (start time plus a hash of the source) and a
`schema` number. Filenames are not identity - two meetings can start in the
same minute and files get renamed - so anything cross-referencing meetings
should join on `meeting_id`.

The Markdown transcript marks each change of speaker with `[mm:ss]`, so a
reader or a model can point at a moment in the recording.

Once the transcript is safely on disk, a model is asked to pull the meeting
apart into `decisions`, `action_items`, `topics` and `open_questions`, and the
answer is folded into the `.json` as `analysis`. Each decision carries
`changed_from`, filled in only when the transcript itself says a position was
revised - which is what makes it possible to ask whether a call overturned
something agreed earlier somewhere else.

`post_meeting_analysis` picks the provider: `local` (Ollama, nothing leaves the
machine), `claude`, `openai`, `gemini`, or `off`. The pass runs after the files
are written and its failure is logged, never fatal: a meeting captured but not
analysed beats a meeting lost to an unavailable model. Hosted models are
noticeably better at this than a small local one, so `local` is the private
default rather than the accurate one.

`index.jsonl` gets an appended line per meeting (`"event": "meeting"`) with
times, duration, source, other party, word count and the transcript's
filename, and a second line once the analysis finishes (`"event": "analysis"`)
carrying the summary and how many decisions were found. The file is only ever
appended to, so a later line adds to a meeting rather than rewriting it. A folder of several
hundred transcripts stays searchable without opening any of them.

### While a meeting runs

The transcript is written to `data\transcripts\<time>.partial.md` every few
seconds and replaced by the finished file when the meeting ends. A crash, a
power cut or a killed process therefore costs the last few seconds rather than
the whole meeting.

On the next start, any `.partial.md` still lying around is promoted to
`<time>-interrupted.md` with a note saying the recording was cut short - a
meeting that ended badly still ends up readable instead of sitting there as a
half-file.

Captured audio is released as soon as Whisper has read it; nothing keeps the
recording in memory for the length of the meeting.

## Security notes

- **Meeting detection polls every 12 seconds**, and each poll only lists
  processes and reads one registry value. A tighter loop of process
  enumeration plus registry reads is the pattern endpoint security scores as
  snooping; twelve seconds is well below any noticeable delay in starting a
  recording. Window titles are read only at the moment a meeting is detected,
  never on every poll.
- **The executable carries description and copyright metadata**, so it does not
  look like an anonymous unsigned binary.

- **API keys are encrypted at rest.** Keys for Claude, OpenAI, Gemini and
  ElevenLabs are wrapped with Windows DPAPI before they are written to
  `platypus.sqlite`, so the value is tied to the current Windows user. A
  database copied to another machine or another user account will not decrypt
  them - they are simply blank there and must be re-entered. Keys saved before
  this change are re-encrypted the next time they are saved.

- **The executable is not signed.** SmartScreen warns on first launch, and an
  unsigned recorder that captures audio and watches other processes looks, to
  an endpoint security product, much like something unwanted. On managed or
  corporate machines, sign the build and clear it with whoever runs security
  before deploying. `tauri.conf.json` has the hook
  (`bundle.windows.certificateThumbprint`) for a signing certificate.

- **Prefer a real install location over a USB stick.** A removable drive full
  of meeting transcripts is what data-loss-prevention tooling is built to
  flag, and the drive is unencrypted unless you encrypt it. Copy the folder to
  a normal disk (ideally BitLocker-protected) rather than running it from the
  stick when the recordings matter.

- **Recording without telling the other participants** is a policy and consent
  question, not a technical one, and no setting here addresses it. Teams shows
  a banner when *it* records; this does not. Whether that is acceptable is
  yours (and your organisation's) to decide.

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
