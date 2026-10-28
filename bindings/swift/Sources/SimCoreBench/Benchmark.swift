import Darwin
import Foundation
import SimCoreKit

/// Phase 1 bridge benchmark: N dummy units, timed from Swift across the C boundary.
public enum BridgeBenchmark {
    public struct Stats: Sendable {
        public let name: String
        public let medianNs: Double
        public let p99Ns: Double
        public let minNs: Double
        public let maxNs: Double
    }

    public struct Report: Sendable {
        public let units: UInt32
        public let iterations: Int
        public let frameBytes: Int
        public let stats: [Stats]
        /// `phys_footprint` in bytes: before world creation, after creation (and one warm-up
        /// frame), and after the timed iterations.
        public let footprintBefore: UInt64
        public let footprintAfterCreate: UInt64
        public let footprintAfterRun: UInt64
        public let checksum: Double

        public func formatted() -> String {
            var s = "units: \(units) (+1 command node), iterations: \(iterations), "
            s += "frame: \(frameBytes) bytes\n\n"
            s += "| Measurement | Median | p99 | Min | Max |\n|---|---|---|---|---|\n"
            for st in stats {
                s += "| \(st.name) | \(ms(st.medianNs)) | \(ms(st.p99Ns)) | "
                s += "\(ms(st.minNs)) | \(ms(st.maxNs)) |\n"
            }
            s += "\n| phys_footprint | Bytes | MiB |\n|---|---|---|\n"
            for (label, v) in [
                ("before world creation", footprintBefore),
                ("after creation + first frame", footprintAfterCreate),
                ("after \(iterations) iterations", footprintAfterRun),
            ] {
                s += "| \(label) | \(v) | \(mib(v)) |\n"
            }
            let growth = Int64(bitPattern: footprintAfterRun &- footprintAfterCreate)
            s += "\ngrowth during iterations: \(growth) bytes\n"
            s += "checksum: \(checksum)\n"
            return s
        }
    }

    /// Runs the benchmark. Each iteration measures, in order:
    /// 1. `step(1)`;
    /// 2. `sc_world_frame` (inside `withFrame`, before the body runs);
    /// 3. reading every unit from Swift (summing x + y + z);
    /// 4. copying the whole frame into a preallocated contiguous buffer (Metal upload stand-in).
    public static func run(units: UInt32 = 50_000, iterations: Int = 300, warmup: Int = 10)
        -> Report
    {
        precondition(iterations > 0)
        let before = physFootprint()
        guard let world = SimWorld(seed: 0x5EED, dummyUnits: units) else {
            fatalError("sc_world_create_dummy returned NULL")
        }
        let frameBytes = world.withFrame { $0.count * MemoryLayout<ScUnitView>.stride }
        // Stand-in for an MTLBuffer's contents: allocated once, page-aligned, reused.
        let upload = UnsafeMutableRawPointer.allocate(byteCount: frameBytes, alignment: 16_384)
        defer { upload.deallocate() }
        // Fault the pages in now, so their first touch isn't counted as growth during the run.
        memset(upload, 0, frameBytes)
        // Sample storage, written (so faulted in) before the "after creation" measurement.
        // Separate allocations: `var b = a` would share one copy-on-write buffer, and the
        // first write in the loop would then copy it and count as growth.
        func samples() -> [UInt64] { [UInt64](repeating: 0, count: iterations) }
        var step = samples(), frame = samples(), read = samples()
        var copy = samples(), frameAndRead = samples()
        let afterCreate = physFootprint()

        var checksum = 0.0
        for i in 0..<(warmup + iterations) {
            let t0 = now()
            world.step(1)
            let t1 = now()
            var t2: UInt64 = 0, t3: UInt64 = 0, t4: UInt64 = 0
            world.withFrame { units in
                t2 = now()
                var sum: Float = 0
                for u in units {
                    sum += u.x + u.y + u.z
                }
                t3 = now()
                if let base = units.baseAddress {
                    memcpy(upload, base, units.count * MemoryLayout<ScUnitView>.stride)
                }
                t4 = now()
                checksum += Double(sum)
            }
            blackHole(upload)
            if i >= warmup {
                let k = i - warmup
                step[k] = t1 - t0
                frame[k] = t2 - t1
                read[k] = t3 - t2
                frameAndRead[k] = t3 - t1
                copy[k] = t4 - t3
            }
        }
        let afterRun = physFootprint()
        return Report(
            units: units,
            iterations: iterations,
            frameBytes: frameBytes,
            stats: [
                stats("`step(1)` from Swift", step),
                stats("`sc_world_frame` (rebuild, no copy)", frame),
                stats("Swift reads every unit (sum x+y+z)", read),
                stats("frame + read (total)", frameAndRead),
                stats("memcpy frame to upload buffer", copy),
            ],
            footprintBefore: before,
            footprintAfterCreate: afterCreate,
            footprintAfterRun: afterRun,
            checksum: checksum
        )
    }

    /// Process `phys_footprint` from `task_info(TASK_VM_INFO)`, in bytes (0 on failure).
    public static func physFootprint() -> UInt64 {
        var info = task_vm_info_data_t()
        // TASK_VM_INFO_COUNT and mach_task_self() are macros Swift doesn't import; the SDK
        // marks the underlying `mach_task_self_` global `__swift_nonisolated_unsafe`.
        var count = mach_msg_type_number_t(
            MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<integer_t>.size)
        let kr = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count)
            }
        }
        return kr == KERN_SUCCESS ? info.phys_footprint : 0
    }

    @inline(__always)
    static func now() -> UInt64 {
        clock_gettime_nsec_np(CLOCK_UPTIME_RAW)
    }

    /// Keeps the copy from being optimised away.
    @inline(never)
    static func blackHole(_ p: UnsafeMutableRawPointer) {
        _ = p.load(as: UInt8.self)
    }

    static func stats(_ name: String, _ samples: [UInt64]) -> Stats {
        // Not `.map(Double.init)`: unapplied, that resolves to `Double.init(bitPattern:)`.
        let s = samples.sorted().map { Double($0) }
        let n = s.count
        let median = n % 2 == 1 ? s[n / 2] : (s[n / 2 - 1] + s[n / 2]) / 2
        // Nearest-rank p99.
        let p99 = s[min(n - 1, Int((0.99 * Double(n)).rounded(.up)) - 1)]
        return Stats(name: name, medianNs: median, p99Ns: p99, minNs: s[0], maxNs: s[n - 1])
    }
}

private func ms(_ ns: Double) -> String {
    let v = ns / 1_000_000
    return v < 0.1 ? String(format: "%.4f ms", v) : String(format: "%.3f ms", v)
}

private func mib(_ bytes: UInt64) -> String {
    String(format: "%.2f", Double(bytes) / 1_048_576)
}
