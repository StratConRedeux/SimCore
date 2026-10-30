#!/usr/bin/env bash
# Cuts a release locally: builds and zips the XCFramework, points the root Package.swift at the
# zip's GitHub Release asset, runs the tests, commits "Release <version>" and creates an
# annotated tag. It never pushes and never creates the GitHub Release: it prints the commands
# for a human to run. See docs/releasing.md.
#
# Usage: scripts/release.sh [--dry-run] <version>
#   <version>  semantic version without a "v" prefix, for example 0.2.0 or 0.2.0-rc.1
#   --dry-run  do everything in a temporary clone; the real repo, its tags and index are untouched
set -euo pipefail

usage() { echo "usage: $0 [--dry-run] <version>" >&2; exit 2; }
die() { echo "error: $*" >&2; exit 1; }

DRY_RUN=0
VERSION=""
for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY_RUN=1 ;;
        -*) usage ;;
        *) [[ -z "$VERSION" ]] || usage; VERSION="$arg" ;;
    esac
done
[[ -n "$VERSION" ]] || usage

REPO="StratConRedeux/SimCore"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"

# --- Preconditions (checked on the real repo, in both modes) ---
SEMVER='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$'
[[ "$VERSION" =~ $SEMVER ]] || die "'$VERSION' is not a semantic version (X.Y.Z, no 'v')"
[[ -z "$(git status --porcelain)" ]] || die "working tree is not clean"
[[ "$(git symbolic-ref --quiet --short HEAD || true)" == main ]] || die "not on main"
! git rev-parse --quiet --verify "refs/tags/$VERSION" >/dev/null \
    || die "tag $VERSION already exists"
# A "Release ..." commit at HEAD is either already tagged (nothing new to release) or left over
# from a run that failed after committing; rerunning would stack a second release commit.
[[ "$(git log -1 --format=%s)" != "Release "* ]] \
    || die "HEAD is a release commit; if it is untagged, undo it with: git reset --hard HEAD~1"
! grep -qF "/releases/download/$VERSION/" Package.swift \
    || die "Package.swift already points at the $VERSION asset"
# Whether main is up to date with origin is left to the human (docs/releasing.md): checking
# would need the network.

if (( DRY_RUN )); then
    # The tree is clean, so a clone of HEAD is an exact copy. Without a remote, nothing done in
    # it can reach the real repo.
    WORK="$(mktemp -d "${TMPDIR:-/tmp}/simcore-release.XXXXXX")"
    echo "==> dry run in $WORK/SimCore"
    git clone --quiet --no-hardlinks "$ROOT" "$WORK/SimCore"
    cd "$WORK/SimCore"
    git remote remove origin
    git checkout --quiet main
    ROOT="$PWD"
fi

ZIP="$ROOT/build/SimCore.xcframework.zip"
ASSET_URL="https://github.com/$REPO/releases/download/$VERSION/SimCore.xcframework.zip"

# --- Build and zip ---
scripts/build-xcframework.sh

# Reproducible zip, so the same commit and toolchain give the same checksum. The staticlibs
# already come out byte-identical, even from different checkout paths, but xcodebuild lists
# the slices in Info.plist in varying order, so sort them. `ditto -c -k` (Apple's usual
# suggestion) stores the files' real mtimes in directory order; here entries are sorted,
# mtimes and modes are fixed (to the commit time of the HEAD the release is cut from, which
# becomes the parent of the Release commit, so `git log -1 --format=%ct <version>^` recomputes
# it), and zip's extra fields (uid/gid, UTC times) are dropped with -X.
EPOCH="$(git log -1 --format=%ct)"
STAMP="$(TZ=UTC date -r "$EPOCH" +%Y%m%d%H%M.%S)"
rm -f "$ZIP"
/usr/bin/python3 -I - "$ROOT/build/SimCore.xcframework/Info.plist" <<'EOF'
import plistlib, sys
path = sys.argv[1]
with open(path, "rb") as f:
    info = plistlib.load(f)
info["AvailableLibraries"].sort(key=lambda lib: lib["LibraryIdentifier"])
with open(path, "wb") as f:
    plistlib.dump(info, f, sort_keys=True)
EOF
(
    cd "$ROOT/build"
    find SimCore.xcframework -type d -exec chmod 755 {} +
    find SimCore.xcframework -type f -exec chmod 644 {} +
    find SimCore.xcframework -exec env TZ=UTC touch -h -t "$STAMP" {} +
    find SimCore.xcframework -print | LC_ALL=C sort | TZ=UTC zip -q -X -@ "$ZIP"
)
CHECKSUM="$(swift package compute-checksum "$ZIP")"
echo "==> $ZIP"
echo "  checksum $CHECKSUM"

# --- Point the root manifest at the release asset ---
# From here until the commit, any exit (including `die`) restores Package.swift from HEAD, so
# the tree is clean for a rerun. A dry run's clone is throwaway, so it needs no restoring.
trap 'exit 130' INT TERM
(( DRY_RUN )) || trap 'git checkout HEAD -- Package.swift' EXIT
sed -E -i '' \
    -e "s#^( *)url: \"https://github\\.com/[^\"]*/SimCore\\.xcframework\\.zip\",#\\1url: \"$ASSET_URL\",#" \
    -e "s#^( *)checksum: \"[0-9a-f]{64}\"#\\1checksum: \"$CHECKSUM\"#" \
    Package.swift
grep -qF "url: \"$ASSET_URL\"," Package.swift || die "could not rewrite the url in Package.swift"
grep -qF "checksum: \"$CHECKSUM\"" Package.swift || die "could not rewrite the checksum"
swift package dump-package | grep -qF "\"$CHECKSUM\"" || die "root Package.swift does not parse"

# --- Tests ---
echo "==> cargo test --workspace"
cargo test --locked --quiet --workspace
echo "==> C conformance against the release build"
cargo test --locked --quiet --release -p simcore-ffi --test c_conformance
echo "==> swift test (bindings/swift, local XCFramework)"
(cd bindings/swift && swift test)
# The manifest being tagged, against the zip being uploaded.
scripts/check-root-manifest.sh "$ZIP"

# --- Commit and tag (local only) ---
git add Package.swift
git commit --quiet -m "Release $VERSION"
# If tagging fails, drop the release commit too, so the rerun starts from the same HEAD and
# gives the same zip.
(( DRY_RUN )) || trap 'git reset --quiet --keep HEAD~1' EXIT
git tag -a "$VERSION" -m "SimCore $VERSION"
trap - EXIT INT TERM
echo "==> committed $(git rev-parse --short HEAD) \"Release $VERSION\" and tagged $VERSION"

if (( DRY_RUN )); then
    echo
    echo "Dry run complete. Nothing in the real repo changed. The release commit, tag and zip"
    echo "are in $ROOT (delete it when done). A real run would leave these commands to run:"
fi
cat <<EOF

Nothing has been pushed or published. To publish, run these two commands back to back:

  git push origin main $VERSION
  gh release create $VERSION build/SimCore.xcframework.zip --repo $REPO --verify-tag \\
      --title "SimCore $VERSION" --notes "SimCoreKit $VERSION. XCFramework checksum: $CHECKSUM"

Until the second one finishes, resolving $VERSION fails: the tagged Package.swift points at
$ASSET_URL
which does not exist yet. See docs/releasing.md.
EOF
