<#
.SYNOPSIS
    Updates an installed portable Platypus to the current build.

.DESCRIPTION
    Replaces the program files in an existing installation while leaving the
    `data` folder untouched: notes, transcripts, Whisper models and settings
    all live there, and losing them to an update would be worse than running an
    old build.

    Run it after building (scripts\build-portable.cmd or build-portable-cuda.cmd).

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\update-portable.ps1 -Target D:\Platypus

.EXAMPLE
    # Close a running instance first instead of refusing to write
    powershell -ExecutionPolicy Bypass -File scripts\update-portable.ps1 -Target D:\Platypus -StopRunning
#>
[CmdletBinding()]
param(
    # Installed folder to update - the one holding Platypus.exe and data\.
    [Parameter(Mandatory = $true)]
    [string]$Target,

    # Freshly built package. Defaults to the newest folder under dist-portable*.
    [string]$Source,

    # Close a running Platypus before replacing files.
    [switch]$StopRunning
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = Split-Path -Parent $PSScriptRoot
Push-Location $repoRoot
try {
    if (-not $Source) {
        $candidate = Get-ChildItem -Path $repoRoot -Directory -Filter 'dist-portable*' -ErrorAction SilentlyContinue |
            ForEach-Object { Get-ChildItem -Path $_.FullName -Directory -ErrorAction SilentlyContinue } |
            Sort-Object LastWriteTime -Descending |
            Select-Object -First 1
        if (-not $candidate) { throw 'No built package found. Run scripts\build-portable.cmd first.' }
        $Source = $candidate.FullName
    }

    if (-not (Test-Path (Join-Path $Source 'Platypus.exe'))) {
        throw "No Platypus.exe in $Source"
    }
    if (-not (Test-Path $Target)) {
        throw "Target folder does not exist: $Target. For a first install, copy the package there instead."
    }

    $installed = Join-Path $Target 'VERSION.txt'
    if (Test-Path $installed) {
        Write-Host '==> Installed:'
        Get-Content $installed | ForEach-Object { Write-Host "    $_" }
    }
    Write-Host '==> New build:'
    Get-Content (Join-Path $Source 'VERSION.txt') | ForEach-Object { Write-Host "    $_" }

    $running = Get-Process -Name 'Platypus' -ErrorAction SilentlyContinue
    if ($running) {
        if (-not $StopRunning) {
            throw 'Platypus is running. Close it, or rerun with -StopRunning. Never do this during a meeting - a recording in progress would be lost.'
        }
        Write-Host '==> Closing the running instance'
        $running | Stop-Process -Force
        Start-Sleep -Seconds 2
    }

    # Everything except data\ is replaceable; data\ is the user's.
    Write-Host "==> Updating $Target"
    foreach ($item in Get-ChildItem -Path $Source) {
        if ($item.Name -ieq 'data') { continue }
        Copy-Item $item.FullName -Destination $Target -Recurse -Force
        Write-Host "    $($item.Name)"
    }

    # A bundled model is worth copying in, but only when the installation has
    # none - never overwrite a model the machine already downloaded.
    $sourceModels = Join-Path $Source 'data\models'
    $targetModels = Join-Path $Target 'data\models'
    if (Test-Path $sourceModels) {
        New-Item -ItemType Directory -Path $targetModels -Force | Out-Null
        foreach ($model in Get-ChildItem -Path $sourceModels -Filter '*.bin' -ErrorAction SilentlyContinue) {
            $destination = Join-Path $targetModels $model.Name
            if (-not (Test-Path $destination)) {
                Copy-Item $model.FullName $destination
                Write-Host "    data\models\$($model.Name)"
            }
        }
    }

    Write-Host ''
    Write-Host "Updated. data\ was left as it was - notes, transcripts and models are intact."
}
finally {
    Pop-Location
}
