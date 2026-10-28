#!/usr/bin/env bash
# Builds build/SimCore.xcframework: release staticlibs of simcore-ffi for iOS device,
# iOS simulator and macOS (all arm64), with include/simcore.h and a SimCoreC module map.
#
# Usage: scripts/build-xcframework.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"

# Match the Swift package's platforms so the linker sees consistent minimum OS versions.
export IPHONEOS_DEPLOYMENT_TARGET=27.0
export MACOSX_DEPLOYMENT_TARGET=26.0

TARGETS=(aarch64-apple-ios aarch64-apple-ios-sim aarch64-apple-darwin)
BUILD="$ROOT/build"
OUT="$BUILD/SimCore.xcframework"
HEADERS="$BUILD/headers"

for t in "${TARGETS[@]}"; do
    echo "==> cargo rustc --release --target $t"
    # `cargo rustc --crate-type staticlib` builds only the staticlib. A plain `cargo build`
    # also emits the rlib, and with that combination the archive additionally carries every
    # upstream crate's bitcode-embedded objects (about 4x larger, never linked).
    cargo rustc --locked --quiet --release -p simcore-ffi --lib \
        --crate-type staticlib --target "$t"
done

rm -rf "$HEADERS" "$OUT"
mkdir -p "$HEADERS"
cp include/simcore.h "$HEADERS/"
cat > "$HEADERS/module.modulemap" <<'EOF'
module SimCoreC {
    header "simcore.h"
    export *
}
EOF

args=()
for t in "${TARGETS[@]}"; do
    args+=(-library "target/$t/release/libsimcore_ffi.a" -headers "$HEADERS")
done
xcodebuild -create-xcframework "${args[@]}" -output "$OUT" >/dev/null

echo "==> $OUT"
for lib in "$OUT"/*/libsimcore_ffi.a; do
    slice="$(basename "$(dirname "$lib")")"
    bytes="$(stat -f %z "$lib")"
    printf '  %-28s %10d bytes (%s)\n' "$slice" "$bytes" "$(du -h "$lib" | cut -f1 | tr -d ' ')"
done
