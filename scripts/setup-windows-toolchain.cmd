@echo off
rem One-shot toolchain setup for building the portable Windows package.
rem Run in an elevated console, then open a NEW console so the environment
rem variables the installers set are picked up.
setlocal

echo ==================================================================
echo  Installing the build toolchain with winget
echo ==================================================================
echo.

where winget >nul 2>&1 || goto :nowinget

echo [1/5] Node LTS
winget install --id OpenJS.NodeJS.LTS --accept-source-agreements --accept-package-agreements

echo [2/5] Rust
winget install --id Rustlang.Rustup --accept-source-agreements --accept-package-agreements

echo [3/5] CMake
winget install --id Kitware.CMake --accept-source-agreements --accept-package-agreements

echo [4/5] LLVM (libclang, needed by bindgen)
winget install --id LLVM.LLVM --accept-source-agreements --accept-package-agreements

echo [5/5] Visual Studio Build Tools with the C++ workload
winget install --id Microsoft.VisualStudio.2022.BuildTools --accept-source-agreements --accept-package-agreements --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"

echo.
echo Optional, only for the GPU build (several GB):
echo    winget install --id Nvidia.CUDA
echo.
echo ==================================================================
echo  Done. Open a NEW console, then run:
echo    scripts\build-portable.cmd        (CPU)
echo    scripts\build-portable-cuda.cmd   (NVIDIA GPU)
echo ==================================================================
pause
exit /b 0

:nowinget
echo [ERROR] winget not found. Install "App Installer" from the Microsoft Store.
pause
exit /b 1
