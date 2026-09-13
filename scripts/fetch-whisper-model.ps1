<#
.SYNOPSIS
    Downloads a Whisper model into a portable Platypus folder.

.DESCRIPTION
    Pulls the ggml model straight from Hugging Face into `data\models` next to
    Platypus.exe, under the exact filename the app expects. The download
    resumes, so an interrupted transfer can be restarted without starting over.

    The app downloads the model by itself on first use too; this script is for
    doing it up front, or on a machine where you would rather control when a
    multi-gigabyte transfer happens.

.EXAMPLE
    # Run from inside the unpacked portable folder
    powershell -ExecutionPolicy Bypass -File fetch-whisper-model.ps1

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File fetch-whisper-model.ps1 -Model large-v3-turbo
#>
[CmdletBinding()]
param(
    [ValidateSet('large-v3', 'large-v3-turbo', 'distil-large-v3.5')]
    [string]$Model = 'large-v3',

    # Folder holding Platypus.exe. Defaults to the current directory.
    [string]$Destination = '.'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$models = @{
    'large-v3'          = @{ file = 'ggml-large-v3.bin';          url = 'https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3.bin' }
    'large-v3-turbo'    = @{ file = 'ggml-large-v3-turbo.bin';    url = 'https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin' }
    'distil-large-v3.5' = @{ file = 'ggml-distil-large-v3.5.bin'; url = 'https://huggingface.co/distil-whisper/distil-large-v3.5-ggml/resolve/main/ggml-model.bin' }
}

$Destination = (Resolve-Path $Destination).Path
if (-not (Test-Path (Join-Path $Destination 'Platypus.exe'))) {
    Write-Warning "No Platypus.exe in $Destination - make sure this is the portable folder."
}

$target = Join-Path $Destination 'data\models'
New-Item -ItemType Directory -Path $target -Force | Out-Null

$entry = $models[$Model]
$dest = Join-Path $target $entry.file

Write-Host "==> $Model -> $dest"
Write-Host '    Several GB. Interrupt with Ctrl+C and rerun to resume.'

& curl.exe -L -C - --retry 5 --retry-delay 5 -o $dest $entry.url
if ($LASTEXITCODE -ne 0) {
    throw "Download failed (curl exit $LASTEXITCODE). Rerun this script to resume."
}

$sizeMb = [math]::Round((Get-Item $dest).Length / 1MB)
if ($sizeMb -lt 100) { throw "Downloaded file looks truncated ($sizeMb MB)." }

Write-Host ''
Write-Host "Done: $($entry.file) ($sizeMb MB)"
Write-Host "In the app, pick $Model as the local transcription model."
