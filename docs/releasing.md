# Releasing

SwiftPM consumers depend on the repo root and pin to a tag:

```swift
.package(url: "https://github.com/StratConRedeux/SimCore.git", exact: "X.Y.Z")
// target dependency: .product(name: "SimCoreKit", package: "SimCore")
```

## Two manifests
- `Package.swift` (root): what consumers resolve. Product `SimCoreKit` only. The `SimCore` binary target is a GitHub Release asset (`url:` + `checksum:`), and the Swift targets use the sources under `bindings/swift` by path, so nothing is duplicated. On `main` between releases it may still point at the previous release's asset, or a placeholder; consumers pin a tag, so this doesn't matter.
- `bindings/swift/Package.swift`: local development. Its binary target is the path `build/SimCore.xcframework` made by `scripts/build-xcframework.sh`, and it adds the `simcore-bench` executable.

Never edit the root `url` or `checksum` by hand; `scripts/release.sh` writes them.

## Steps
1. On a clean `main`, up to date with `origin/main` (check this yourself, for example `git fetch origin && git status -sb`; the script works offline and doesn't), rehearse:
   ```sh
   scripts/release.sh --dry-run X.Y.Z
   ```
   This clones the repo into a temporary directory and does the whole release there (build, zip, checksum, manifest rewrite, Rust and Swift tests, root manifest check, commit, tag). The real repo, its index and its tags are not touched. The clone's path is printed; delete it afterwards.
2. Cut the release locally:
   ```sh
   scripts/release.sh X.Y.Z
   ```
   Preconditions: clean tree, on `main`, a semantic version without a `v` prefix, no local tag of that name yet, and HEAD not already a `Release ...` commit. The script then:
   1. sets the Cargo workspace version (`[workspace.package]` in `Cargo.toml`, and `Cargo.lock`) to the release version, so the crates carry the version of the tag they ship in;
   2. builds `build/SimCore.xcframework` with `scripts/build-xcframework.sh`;
   3. zips it reproducibly to `build/SimCore.xcframework.zip` (see Reproducibility below);
   4. runs `swift package compute-checksum` on the zip;
   5. rewrites the root `Package.swift` `url` (to `.../releases/download/X.Y.Z/SimCore.xcframework.zip`) and `checksum`;
   6. runs `cargo test --workspace`, `swift test` in `bindings/swift`, and `scripts/check-root-manifest.sh`, which builds and tests the root manifest against the local zip in a scratch copy;
   7. commits `Release X.Y.Z` and creates the annotated tag `X.Y.Z`.

   It never pushes and never creates the GitHub Release. If any step fails, it puts the repo back as it was: before the commit it restores `Cargo.toml`, `Cargo.lock` and `Package.swift`, and if tagging fails it drops the release commit. Fix the cause and rerun.
3. Publish, running the two commands it prints back to back:
   ```sh
   git push origin main X.Y.Z
   gh release create X.Y.Z build/SimCore.xcframework.zip --repo StratConRedeux/SimCore --verify-tag --title "SimCore X.Y.Z" --notes "..."
   ```
   Upload the zip from this same run: its checksum is the one in the tagged manifest.
4. Check from a consumer: resolve the new tag (`swift package resolve`, or File > Packages > Resolve in Xcode) and build.

## The window between push and upload
The tagged `Package.swift` names an asset that exists only once `gh release create` has uploaded it. Between `git push` and the end of the upload, resolving the new tag fails with a download error (HTTP 404). Only someone resolving that exact tag in those seconds is affected; retrying after the upload works. Run the two commands back to back, and if the upload fails, rerun `gh release create` (or `gh release upload X.Y.Z build/SimCore.xcframework.zip`) rather than moving the tag.

## If something goes wrong
- `release.sh` failed: it restores the tree itself. If it was killed outright (for example `kill -9`) before committing, run `git checkout HEAD -- Package.swift`; if it stopped after committing, use the next bullet.
- Before pushing: undo locally with `git tag -d X.Y.Z 2>/dev/null; git reset --hard HEAD~1` (this works whether or not the tag was made), fix, and rerun.
- After pushing: don't move or reuse a published tag (SwiftPM caches resolved versions and checksums). Release a new patch version instead.
- Checksum mismatch errors from consumers mean the uploaded zip isn't the one from the release run. Re-upload that run's `build/SimCore.xcframework.zip` with `gh release upload --clobber`.

## Reproducibility
The same commit built with the same Rust and Xcode toolchains gives the same zip and checksum, even from a different checkout path (a dry run in a temporary clone matches the real run). The staticlibs already come out byte-identical; `release.sh` sorts the slice list in the XCFramework's `Info.plist`, which `xcodebuild` writes in varying order, and zips with sorted entries, fixed modes and mtimes and no extra fields. The mtime is the commit time of the HEAD the release is cut from, which is the parent of the `Release` commit, so `git log -1 --format=%ct X.Y.Z^` recomputes it from the tag. A different toolchain version will give a different checksum, so always upload the zip from the run that made the tag.

## Checking the root manifest without publishing
```sh
scripts/build-xcframework.sh
# zip as release.sh does, or reuse the zip from a dry run, then:
scripts/check-root-manifest.sh path/to/SimCore.xcframework.zip [copy-dir]
```
This copies the tree (without `crates/`, `target/`, `build/` or `.git`) to a scratch directory, swaps the binary target for `path: "SimCore.xcframework.zip"`, and runs `swift build` and `swift test`. With `copy-dir`, the copy is kept, so a scratch consumer can depend on it with `.package(path:)`.
