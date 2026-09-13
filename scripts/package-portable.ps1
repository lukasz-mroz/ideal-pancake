<#
.SYNOPSIS
    Assembles a portable Windows build of Platypus Notes.

.DESCRIPTION
    Takes the release binary produced by `cargo build --release --features
    "custom-protocol portable"` and packages it into a self-contained folder
    (and .zip) that runs from anywhere - a USB stick included - keeping all
    user data in a `data` folder next to the executable.

    Optionally bundles a Whisper model and the offline WebView2 runtime
    installer so the result works with no internet connection at all.

.EXAMPLE
    # Full offline bundle (default): Whisper large-v3-turbo + WebView2 offline
    powershell -ExecutionPolicy Bypass -File scripts\package-portable.ps1

.EXAMPLE
    # Slim bundle - model and runtime are fetched on first use
    powershell -ExecutionPolicy Bypass -File scripts\package-portable.ps1 -WhisperModel none -IncludeWebView2:$false
#>
[CmdletBinding()]
param(
    # Whisper model to ship inside the package.
    [ValidateSet('large-v3-turbo', 'large-v3', 'distil-large-v3.5', 'none')]
    [string]$WhisperModel = 'large-v3-turbo',

    # Bundle the offline WebView2 runtime installer (~150 MB).
    [bool]$IncludeWebView2 = $true,

    # Where the package is assembled.
    [string]$OutDir = 'dist-portable',

    # Compiled binary; defaults to the cargo release output.
    [string]$ExePath = 'src-tauri\target\release\platypus_notes.exe',

    # Copy the CUDA runtime DLLs next to the executable (for builds made with
    # the `cuda` cargo feature).
    [switch]$IncludeCudaRuntime,

    # Skip creating the .zip (folder only).
    [switch]$NoZip
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = Split-Path -Parent $PSScriptRoot
Push-Location $repoRoot
try {
    if (-not (Test-Path $ExePath)) {
        throw "Binary not found: $ExePath`nBuild it first with:`n  npm install; npm run build`n  cargo build --release --features `"custom-protocol portable`" --manifest-path src-tauri\Cargo.toml"
    }

    $version = (Select-String -Path 'src-tauri\tauri.conf.json' -Pattern '"version"\s*:\s*"([^"]+)"' |
        Select-Object -First 1).Matches[0].Groups[1].Value
    $stageName = "Platypus-Portable-$version-win-x64"
    $stage = Join-Path $OutDir $stageName

    Write-Host "==> Staging $stage"
    if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
    New-Item -ItemType Directory -Path $stage -Force | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $stage 'data\models') -Force | Out-Null

    Copy-Item $ExePath (Join-Path $stage 'Platypus.exe') -Force

    # Only needed on older toolchains; harmless to carry along when present.
    $loader = Join-Path (Split-Path -Parent $ExePath) 'WebView2Loader.dll'
    if (Test-Path $loader) { Copy-Item $loader $stage -Force }

    # ------------------------------------------------------------ CUDA runtime
    if ($IncludeCudaRuntime) {
        if (-not $env:CUDA_PATH) {
            throw 'CUDA runtime requested but CUDA_PATH is not set. Install the CUDA Toolkit and reopen the shell.'
        }
        $cudaBin = Join-Path $env:CUDA_PATH 'bin'
        if (-not (Test-Path $cudaBin)) { throw "CUDA bin folder not found: $cudaBin" }

        $copied = 0
        foreach ($pattern in @('cudart64_*.dll', 'cublas64_*.dll', 'cublasLt64_*.dll')) {
            foreach ($dll in Get-ChildItem -Path $cudaBin -Filter $pattern -ErrorAction SilentlyContinue) {
                Copy-Item $dll.FullName $stage -Force
                $copied++
            }
        }
        if ($copied -eq 0) { throw "No CUDA runtime DLLs found in $cudaBin" }
        Write-Host "    CUDA runtime: $copied DLL(s) from $cudaBin"
    }

    # Helper scripts the target machine may need: pulling a Whisper model, and
    # the CUDA runtime DLLs for a GPU build that was packaged without them.
    foreach ($helper in 'fetch-whisper-model.ps1', 'fetch-cuda-runtime.ps1') {
        $source = Join-Path $PSScriptRoot $helper
        if (Test-Path $source) { Copy-Item $source $stage -Force }
    }

    # Marker file: makes the build portable even if it was compiled without
    # the `portable` cargo feature. Delete it to fall back to %APPDATA%.
    Set-Content -Path (Join-Path $stage 'portable.txt') -Encoding UTF8 -Value @(
        'Platypus runs in portable mode while this file exists.',
        'All data lives in the "data" folder next to Platypus.exe.',
        'Delete this file to store data in %APPDATA% instead.'
    )

    # ---------------------------------------------------------------- Whisper
    if ($WhisperModel -ne 'none') {
        $models = @{
            'large-v3-turbo'    = @{ file = 'ggml-large-v3-turbo.bin'; url = 'https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin' }
            'large-v3'          = @{ file = 'ggml-large-v3.bin';       url = 'https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3.bin' }
            'distil-large-v3.5' = @{ file = 'ggml-distil-large-v3.5.bin'; url = 'https://huggingface.co/distil-whisper/distil-large-v3.5-ggml/resolve/main/ggml-model.bin' }
        }
        $m = $models[$WhisperModel]
        $dest = Join-Path $stage "data\models\$($m.file)"
        Write-Host "==> Downloading Whisper model $WhisperModel (this is a multi-GB download)"
        & curl.exe -L --fail --retry 3 --retry-delay 5 -o $dest $m.url
        if ($LASTEXITCODE -ne 0) { throw "Whisper model download failed (curl exit $LASTEXITCODE)" }
        $sizeMb = [math]::Round((Get-Item $dest).Length / 1MB)
        if ($sizeMb -lt 100) { throw "Downloaded model looks truncated ($sizeMb MB)" }
        Write-Host "    model: $sizeMb MB"
    }

    # --------------------------------------------------------------- WebView2
    if ($IncludeWebView2) {
        $vendor = Join-Path $stage 'vendor'
        New-Item -ItemType Directory -Path $vendor -Force | Out-Null
        $wv = Join-Path $vendor 'MicrosoftEdgeWebView2RuntimeInstallerX64.exe'
        Write-Host '==> Downloading WebView2 offline runtime installer'
        & curl.exe -L --fail --retry 3 --retry-delay 5 -o $wv 'https://go.microsoft.com/fwlink/?linkid=2124701'
        if ($LASTEXITCODE -ne 0 -or -not (Test-Path $wv) -or (Get-Item $wv).Length -lt 10MB) {
            Write-Warning 'Offline WebView2 installer unavailable - falling back to the online bootstrapper.'
            Remove-Item $wv -Force -ErrorAction SilentlyContinue
            & curl.exe -L --fail -o $wv 'https://go.microsoft.com/fwlink/p/?LinkId=2124703'
            if ($LASTEXITCODE -ne 0) { throw "WebView2 installer download failed (curl exit $LASTEXITCODE)" }
        }
        Write-Host "    WebView2: $([math]::Round((Get-Item $wv).Length / 1MB)) MB"
    }

    # --------------------------------------------------------------- Launcher
    $launcher = @'
@echo off
setlocal
cd /d "%~dp0"

rem WebView2 runtime is required by Tauri. Install it once if it is missing.
set "WV2=0"
reg query "HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" /v pv >nul 2>&1 && set "WV2=1"
reg query "HKCU\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" /v pv >nul 2>&1 && set "WV2=1"

if "%WV2%"=="0" (
  if exist "vendor\MicrosoftEdgeWebView2RuntimeInstallerX64.exe" (
    echo Installing the WebView2 runtime, one moment...
    "vendor\MicrosoftEdgeWebView2RuntimeInstallerX64.exe" /silent /install
  ) else (
    echo WebView2 runtime is missing. Download it from:
    echo https://developer.microsoft.com/microsoft-edge/webview2/
    pause
  )
)

start "" "Platypus.exe" %*
endlocal
'@
    Set-Content -Path (Join-Path $stage 'Start Platypus.cmd') -Value $launcher -Encoding ASCII

    # ----------------------------------------------------------------- Readme
    $readme = @"
Platypus Notes - portable build for Windows x64
================================================

Quick start
-----------
1. Unzip this folder anywhere (USB stick, Desktop, D:\Apps - your call).
2. Run "Start Platypus.cmd". It installs the WebView2 runtime if Windows
   does not have it yet, then launches the app. Once WebView2 is present you
   can start Platypus.exe directly.
3. Add your LLM API keys in Settings.

Everything stays in this folder
-------------------------------
data\platypus.sqlite        notes, projects, chats
data\hnsw, data\vectors     vector search indices
data\models                 Whisper speech-to-text models
data\podcasts               generated audio
data\recordings             meeting recordings in progress
data\webview               WebView2 cache and local storage
data\task-mining-resources  screenshots

Nothing is written to %APPDATA% or the registry. To move the app to another
machine, copy the whole folder. To reset the app, delete the data folder.

Want the classic behaviour (data in %APPDATA%)? Delete portable.txt and set
the environment variable PLATYPUS_PORTABLE=0.

Whisper model
-------------
If data\models is empty, the app downloads the model the first time you
transcribe something. To do it up front (and with resume support):

  powershell -ExecutionPolicy Bypass -File fetch-whisper-model.ps1

Add -Model large-v3-turbo for the smaller, faster one. Whatever you pick here
must match the model selected in Settings.

Chatting with your notes still needs whichever LLM you connect - a local
Ollama model keeps the whole thing offline.

Version: $version
"@
    Set-Content -Path (Join-Path $stage 'README.txt') -Value $readme -Encoding UTF8

    # -------------------------------------------------------------------- Zip
    if (-not $NoZip) {
        $zip = Join-Path $OutDir "$stageName.zip"
        Write-Host "==> Creating $zip"
        if (Test-Path $zip) { Remove-Item $zip -Force }
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        [System.IO.Compression.ZipFile]::CreateFromDirectory(
            (Resolve-Path $stage),
            (Join-Path (Resolve-Path $OutDir) "$stageName.zip"),
            [System.IO.Compression.CompressionLevel]::Fastest,
            $true)
        Write-Host "    zip: $([math]::Round((Get-Item $zip).Length / 1MB)) MB"
    }

    Write-Host ''
    Write-Host "Portable build ready: $stage"
}
finally {
    Pop-Location
}
