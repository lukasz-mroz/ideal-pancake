@echo off
rem Builds the portable Windows package. Run from anywhere; paths are relative
rem to the repository root.
setlocal
cd /d "%~dp0.."
set "LOG=%CD%\build-portable.log"
if exist "%LOG%" del "%LOG%"

echo ==================================================================
echo  Platypus portable build (CPU)
echo  Log: %LOG%
echo ==================================================================
echo.

where node  >nul 2>&1 || goto :nonode
where cargo >nul 2>&1 || goto :nocargo
where cmake >nul 2>&1 || goto :nocmake

if not defined LIBCLANG_PATH set "LIBCLANG_PATH=C:\Program Files\LLVM\bin"
if not exist "%LIBCLANG_PATH%\libclang.dll" goto :noclang

echo [1/3] npm install ...
call npm install --no-audit --no-fund >> "%LOG%" 2>&1
if errorlevel 1 goto :fail

echo [2/3] frontend ...
call npm run build >> "%LOG%" 2>&1
if errorlevel 1 (
  echo       tsc failed - building with vite only
  call npx vite build >> "%LOG%" 2>&1
  if errorlevel 1 goto :fail
)

echo [3/3] cargo build - 15-40 minutes on a cold cache ...
cargo build --release --features "custom-protocol portable" --manifest-path src-tauri\Cargo.toml >> "%LOG%" 2>&1
if errorlevel 1 goto :fail

echo       packaging ...
powershell -ExecutionPolicy Bypass -Command "& '.\scripts\package-portable.ps1' -WhisperModel none -IncludeWebView2:$false" >> "%LOG%" 2>&1
if errorlevel 1 goto :fail

echo.
echo ==================================================================
echo  DONE. Package: dist-portable\
echo  The Whisper model is not bundled - the app downloads it on first
echo  use, or pass -WhisperModel large-v3 to scripts\package-portable.ps1
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
