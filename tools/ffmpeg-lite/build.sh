#!/bin/sh
# Builds the decode-only, LGPL ffmpeg.exe that the installer bundles.
#
#   ./build.sh <FFmpeg source dir> <output dir>
#
# Cross-compiles for 64-bit Windows with llvm-mingw (https://github.com/mstorsjo/llvm-mingw)
# on Linux or WSL; put its bin/ on PATH first. Native Windows: run it from an MSYS2
# "UCRT64" shell with mingw-w64-ucrt-x86_64-{gcc,nasm,make} installed and set
# NATIVE=1 (no cross prefix). Set TARGET=linux to build a test copy for this machine.
#
# macOS (also what the GitHub Actions workflow runs): TARGET=macos ARCH=arm64 or
# ARCH=x86_64, on a Mac with the Xcode command line tools; https uses Secure Transport.
# Combine the two into one universal binary with lipo afterwards.
#
# Source used for the bundled build: FFmpeg n7.1.1
#   https://github.com/FFmpeg/FFmpeg/tree/n7.1.1  (unmodified)
set -e
SRC=$(cd "$1" && pwd)
OUT=$(mkdir -p "$2" && cd "$2" && pwd)
HERE=$(cd "$(dirname "$0")" && pwd)
FLAGS=$(sh "$HERE/configure-flags.sh")
NAME="${TARGET:-windows}"
[ "$NAME" = macos ] && NAME="macos-${ARCH:-arm64}"
BUILD="$OUT/build-$NAME"
mkdir -p "$BUILD"
cd "$BUILD"

if [ "${TARGET:-windows}" = macos ]; then
  ARCH=${ARCH:-arm64}
  MIN=${MACOSX_DEPLOYMENT_TARGET:-11.0}
  # shellcheck disable=SC2086
  "$SRC/configure" $FLAGS --enable-securetransport --disable-x86asm \
    --enable-cross-compile --target-os=darwin --arch="$ARCH" \
    --cc="clang -arch $ARCH" \
    --extra-cflags="-mmacosx-version-min=$MIN" \
    --extra-ldflags="-arch $ARCH -mmacosx-version-min=$MIN" \
    --prefix="$OUT/$NAME"
elif [ "${TARGET:-windows}" = linux ]; then
  # shellcheck disable=SC2086
  "$SRC/configure" $FLAGS --prefix="$OUT/linux"
elif [ -n "$NATIVE" ]; then
  # shellcheck disable=SC2086
  "$SRC/configure" $FLAGS --enable-schannel --target-os=mingw32 \
    --extra-ldflags=-static --prefix="$OUT/windows"
else
  # shellcheck disable=SC2086
  "$SRC/configure" $FLAGS --enable-schannel \
    --enable-cross-compile --target-os=mingw32 --arch=x86_64 \
    --cross-prefix=x86_64-w64-mingw32- --cc=x86_64-w64-mingw32-clang \
    --extra-ldflags=-static --prefix="$OUT/windows"
fi
make -j"$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)"
make install
echo "built: $OUT/$NAME/bin/"
