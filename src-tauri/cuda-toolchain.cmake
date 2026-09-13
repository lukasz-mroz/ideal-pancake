# Pins the CUDA architecture whisper.cpp kernels are compiled for.
#
# Needed when building on a machine with no NVIDIA GPU: CMake cannot detect an
# architecture, and the default may not match the card the build will run on.
# 75 = Turing (Quadro T600/T1200, GTX 16xx, RTX 20xx).
#
# Used through CMAKE_TOOLCHAIN_FILE, which the cmake crate passes on to the
# whisper.cpp build - see scripts/build-portable-cuda.cmd.
#
# Other cards: 86 = Ampere (RTX 30xx), 89 = Ada (RTX 40xx). A semicolon-
# separated list builds for several ("75;86;89") at the cost of build time.
set(CMAKE_CUDA_ARCHITECTURES 75 CACHE STRING "" FORCE)
