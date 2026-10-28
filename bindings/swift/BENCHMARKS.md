# Bridge benchmark (Phase 1 gate)

50,000 dummy units plus the command node, driven from Swift through `SimCoreKit` and the C API
(`libsimcore_ffi.a` in `build/SimCore.xcframework`). Each of 300 timed iterations (after 10
warm-up iterations) measures, in order:

1. `world.step(1)`: one `sc_world_step` call;
2. `sc_world_frame`: rebuilding the frame inside `withFrame`, before the body runs. It returns a
   pointer into the engine's buffer, with no copy;
3. Swift reading every unit (summing `x + y + z` into a checksum, so it isn't optimised away);
4. `memcpy` of the whole frame (2,000,040 B) into a preallocated, page-aligned buffer. This is
   the stand-in for a Metal buffer upload. It runs right after the read, so the source is hot in
   cache.

Timer: `clock_gettime_nsec_np(CLOCK_UPTIME_RAW)`. Median and p99 (nearest rank) over 300 samples.
Memory: process `phys_footprint` from `task_info(TASK_VM_INFO)`.

Machine: Apple M6 Mac (12 cores, 16 GB), macOS 27.0 (26A428), Xcode 27.1, Swift 6.4,
rustc 1.99.1. Rust staticlib built in release (`lto = "fat"`, `codegen-units = 1`,
`panic = "abort"`).

## macOS, release (`swift run -c release`)

| Measurement | Median | p99 |
|---|---|---|
| `step(1)` from Swift | 0.052 ms | 0.061 ms |
| `sc_world_frame` (rebuild 50,001 views, no copy) | 0.085 ms | 0.104 ms |
| Swift reads every unit (sum x+y+z) | 0.024 ms | 0.028 ms |
| frame + read (total) | 0.109 ms | 0.129 ms |
| `memcpy` frame to upload buffer (2 MB) | 0.017 ms | 0.021 ms |

All three runs came within ±1 µs on the medians. The p99 varied more between runs: up to 0.12 ms
for `step(1)` in one run.

| `phys_footprint` | Bytes | MiB |
|---|---|---|
| before world creation | 1,884,616 | 1.80 |
| after creation + first frame | 19,431,928 | 18.53 |
| after 300 iterations | 19,497,464 | 18.59 |

The world adds about 16.7 MiB. That covers the ECS (about 10.8 MiB, per
`crates/engine/BENCHMARKS.md`), the 1.9 MiB frame buffer, the 1.9 MiB upload buffer, and
allocator and runtime pages.

**Growth during iterations:** 32 to 64 KiB (2 to 4 pages of 16 KiB) across runs, and it doesn't
scale with iteration count. Runs from 300 to 100,000 iterations stay between 16 and 280 KiB with
no trend. A C harness calling `sc_world_step` and `sc_world_frame` 100,000 times directly shows
+16 KiB of footprint and +0 bytes of malloc heap in use. So the engine and FFI don't grow, and
the residue is page-level noise in the process (stack, runtime, lazily faulted pages).

Because the frame pointer stays the same, the copy-free bridge costs nothing measurable. These
numbers match the engine's own Rust benchmark (`step` ~0.055 ms, `frame()` ~0.085 ms).

## iOS 27.0 simulator, Release (`xcodebuild test`)

> **The simulator is not a phone.** It runs arm64 code natively on the Mac's M6 cores, with the
> Mac's clocks, caches, memory bandwidth and thermal headroom. These numbers show that the
> iOS-simulator slice links and behaves the same. They are **not** iPhone performance; measure
> on a device for that.

iPhone 18 Pro simulator (iOS 27.0), Release configuration, three runs:

| Measurement | Median | p99 |
|---|---|---|
| `step(1)` from Swift | 0.053 ms | 0.071 ms |
| `sc_world_frame` (rebuild, no copy) | 0.085 ms | 0.112 ms |
| Swift reads every unit (sum x+y+z) | 0.025 ms | 0.029 ms |
| frame + read (total) | 0.110 ms | 0.139 ms |
| `memcpy` frame to upload buffer (2 MB) | 0.018 ms | 0.027 ms |

| `phys_footprint` | Bytes | MiB |
|---|---|---|
| before world creation | 25,315,512 | 24.14 |
| after creation + first frame | 34,572,472 | 32.97 |
| after 300 iterations | 34,572,472 | 32.97 |

Growth during iterations: 0 to 16 KiB. The baseline is higher because the benchmark runs inside
the XCTest runner process. World creation adds less here (about 8.8 MiB) because the runner
already has faulted-in heap that the allocator reuses. Use the macOS delta as the size of the
world.

The checksum (11443300032.0) is identical on macOS and the simulator.

## Commands

```sh
# From the SimCore root: build the XCFramework (required by the Swift package).
scripts/build-xcframework.sh

# macOS benchmark (release). Optional args: [units] [iterations].
cd bindings/swift
swift run -c release simcore-bench
swift run -c release simcore-bench 50000 300

# Swift tests on macOS (the benchmark test is skipped unless SIMCORE_BENCH=1).
swift test

# Swift tests on the iOS 27 simulator.
xcodebuild test -scheme SimCoreKit-Package \
  -destination 'platform=iOS Simulator,name=iPhone 18 Pro,OS=27.0'

# Benchmark on the iOS 27 simulator. Release configuration; TEST_RUNNER_ env vars are
# forwarded to the test process with the prefix stripped.
TEST_RUNNER_SIMCORE_BENCH=1 xcodebuild test -scheme SimCoreKit-Package -configuration Release \
  -destination 'platform=iOS Simulator,name=iPhone 18 Pro,OS=27.0' \
  -only-testing:SimCoreKitTests/BridgeBenchmarkTests
```
