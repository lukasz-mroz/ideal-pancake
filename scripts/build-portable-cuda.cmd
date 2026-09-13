@echo off
rem Builds the portable Windows package. Run from anywhere; paths are relative
rem to the repository root.
setlocal
cd /d "%~dp0.."
set "LOG=%CD%\build-portable-cuda.log"
if exist "%LOG%" del "%LOG%"

echo ==================================================================
echo  Platypus portable build (CUDA / NVIDIA GPU)
echo  Log: %LOG%
echo ==================================================================
echo.

where node  >nul 2>&1 || goto :nonode
where cargo >nul 2>&1 || goto :nocargo
where cmake >nul 2>&1 || goto :nocmake

if not defined LIBCLANG_PATH set "LIBCLANG_PATH=C:\Program Files\LLVM\bin"
if not exist "%LIBCLANG_PATH%\libclang.dll" goto :noclang

if not defined CUDA_PATH goto :nocuda
if not exist "%CUDA_PATH%\bin\nvcc.exe" goto :nocuda

echo CUDA_PATH: %CUDA_PATH%
nvidia-smi --query-gpu=name,memory.total --format=csv,noheader 2>nul
echo.

echo [1/3] npm install + frontend ...
call npm install --no-audit --no-fund >> "%LOG%" 2>&1
if errorlevel 1 goto :fail
call npm run build >> "%LOG%" 2>&1
if errorlevel 1 goto :fail

echo [2/3] cargo build with CUDA - nvcc kernels take a while, 20-40 minutes ...
cargo build --release --features "custom-protocol portable cuda" --manifest-path src-tauri\Cargo.toml >> "%LOG%" 2>&1
if errorlevel 1 goto :fail

echo [3/3] packaging with the CUDA runtime ...
powershell -ExecutionPolicy Bypass -Command "& '.\scripts\package-portable.ps1' -WhisperModel none -IncludeWebView2:$false -IncludeCudaRuntime -OutDir dist-portable-cuda" >> "%LOG%" 2>&1
if errorlevel 1 goto :fail

echo.
echo ==================================================================
echo  DONE. Package: dist-portable-cuda\
echo.
echo  Note on VRAM: large-v3 in fp16 needs ~3.1 GB. On a 4 GB card that
echo  also drives the desktop, prefer large-v3-turbo if it runs out.
echo ==================================================================
pause
exit /b 0

:nonode
echo [ERROR] node not found - winget install OpenJS.NodeJS.LTS
goto :end

:nocargo
echo [ERROR] cargo not found - winget install Rustlang.Rustup
goto :end

:nocmake
echo [ERROR] cmake not found - winget install Kitware.CMake
goto :end

:noclang
echo [ERROR] libclang.dll not found in "%LIBCLANG_PATH%" - winget install LLVM.LLVM
goto :end

:nocuda
echo [ERROR] CUDA Toolkit not found (CUDA_PATH / nvcc.exe).
echo         winget install Nvidia.CUDA, then open a NEW console.
goto :end

:fail
echo.
echo [ERROR] Build failed. Last lines of the log:
echo ------------------------------------------------------------------
powershell -NoProfile -Command "Get-Content '%LOG%' -Tail 30"
echo ------------------------------------------------------------------
echo Full log: %LOG%

:end
echo.
pause
exit /b 1
