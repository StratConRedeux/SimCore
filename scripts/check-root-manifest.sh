#!/usr/bin/env bash
# Builds and tests the root Package.swift against a local XCFramework zip, without publishing:
# copies the working tree (minus crates/, target/, build/ and .git) to a scratch directory,
# points the binaryTarget there at the zip with `path:`, then runs `swift build` and `swift test`.
#
# Usage: scripts/check-root-manifest.sh <SimCore.xcframework.zip> [copy-dir]
# With copy-dir, the copy is made there and kept (for example to depend on it with
# .package(path:)); otherwise it goes in a temporary directory that is removed on exit.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
[[ $# -ge 1 && $# -le 2 ]] || { echo "usage: $0 <zip> [copy-dir]" >&2; exit 2; }
ZIP="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
[[ -f "$ZIP" ]] || { echo "error: no such file: $1" >&2; exit 1; }

if [[ $# -eq 2 ]]; then
    mkdir -p "$2"
    COPY="$(cd "$2" && pwd -P)"
    # The copy-dir is emptied, so refuse anything that overlaps the repo.
    REAL_ROOT="$(cd "$ROOT" && pwd -P)"
    case "$COPY/" in "$REAL_ROOT/"*) echo "error: copy-dir is inside the repo" >&2; exit 1 ;; esac
    case "$REAL_ROOT/" in "$COPY/"*) echo "error: copy-dir contains the repo" >&2; exit 1 ;; esac
    find "$COPY" -mindepth 1 -maxdepth 1 -exec rm -rf {} +
else
    COPY="$(mktemp -d "${TMPDIR:-/tmp}/simcore-root-manifest.XXXXXX")"
    trap 'rm -rf "$COPY"' EXIT
fi

rsync -a --exclude /.git --exclude /crates --exclude /target --exclude /build \
    --exclude .build --exclude .swiftpm "$ROOT/" "$COPY/"
cp "$ZIP" "$COPY/SimCore.xcframework.zip"

# Swap the release asset for the local zip; everything else in the manifest stays as released.
sed -E -i '' \
    -e 's#^( *)url: "https://github\.com/[^"]*/SimCore\.xcframework\.zip",#\1path: "SimCore.xcframework.zip"#' \
    -e '/^ *checksum: "[0-9a-f]{64}"$/d' \
    "$COPY/Package.swift"
grep -q 'path: "SimCore.xcframework.zip"' "$COPY/Package.swift" \
    || { echo "error: could not rewrite the binaryTarget in Package.swift" >&2; exit 1; }

cd "$COPY"
echo "==> swift build (root manifest, local zip) in $COPY"
swift build
echo "==> swift test (root manifest, local zip)"
swift test
