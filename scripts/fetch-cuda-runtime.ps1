<#
.SYNOPSIS
    Downloads the CUDA runtime DLLs a CUDA-enabled Platypus build needs.

.DESCRIPTION
    A build made with the `cuda` cargo feature links cudart and cuBLAS
    dynamically, so the machine running it needs those DLLs next to the
    executable - but it does NOT need the CUDA Toolkit installed.

    This script fetches them from NVIDIA's own redistributable packages
    published on PyPI (plain zip archives; no Python involved) and drops the
    DLLs into the target folder.

    Note: a CPU build cannot be turned into a GPU build by adding these files.
    The CUDA backend is compiled into the executable.

.EXAMPLE
    # Run from inside the unpacked portable folder
    powershell -ExecutionPolicy Bypass -File fetch-cuda-runtime.ps1

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\fetch-cuda-runtime.ps1 -Destination D:\Platypus-Portable
#>
[CmdletBinding()]
param(
    # Folder holding Platypus.exe. Defaults to the current directory.
    [string]$Destination = '.',

    # CUDA major version the build was made against (12 for CUDA 12.x).
    [ValidateSet('12', '11')]
    [string]$CudaMajor = '12'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Destination = (Resolve-Path $Destination).Path
if (-not (Test-Path (Join-Path $Destination 'Platypus.exe'))) {
    Write-Warning "No Platypus.exe in $Destination - make sure this is the portable folder."
}

$packages = @("nvidia-cuda-runtime-cu$CudaMajor", "nvidia-cublas-cu$CudaMajor")
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("cuda-rt-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $work -Force | Out-Null

try {
    $copied = @()

    foreach ($package in $packages) {
        Write-Host "==> $package"

        # PyPI's JSON API lists the files of the latest release; pick the
        # Windows wheel rather than hard-coding a version number.
        $meta = Invoke-RestMethod -Uri "https://pypi.org/pypi/$package/json" -UseBasicParsing
        $wheel = $meta.urls | Where-Object { $_.filename -like '*win_amd64*.whl' } | Select-Object -First 1
        if (-not $wheel) { throw "No Windows wheel published for $package" }

        $sizeMb = [math]::Round($wheel.size / 1MB)
        Write-Host "    $($wheel.filename) ($sizeMb MB)"

        $zip = Join-Path $work ($wheel.filename -replace '\.whl$', '.zip')
        Invoke-WebRequest -Uri $wheel.url -OutFile $zip -UseBasicParsing

        $unpacked = Join-Path $work ([System.IO.Path]::GetFileNameWithoutExtension($zip))
        Expand-Archive -Path $zip -DestinationPath $unpacked -Force

        foreach ($dll in Get-ChildItem -Path $unpacked -Recurse -Filter '*.dll') {
            Copy-Item $dll.FullName $Destination -Force
            $copied += $dll.Name
        }
    }

    if ($copied.Count -eq 0) { throw 'No DLLs found in the downloaded packages.' }

    Write-Host ''
    Write-Host "Copied into ${Destination}:"
    $copied | Sort-Object -Unique | ForEach-Object { Write-Host "  $_" }
    Write-Host ''
    Write-Host 'Start Platypus and check nvidia-smi during transcription to confirm the GPU is used.'
}
finally {
    Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
}
