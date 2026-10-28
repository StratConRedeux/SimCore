import Foundation
import SimCoreBench
import SimCoreKit
import Testing

/// The frame as raw bytes, copied out while it is valid.
private func frameBytes(_ world: SimWorld) -> [UInt8] {
    world.withFrame { Array(UnsafeRawBufferPointer($0)) }
}

@Suite struct SimWorldTests {
    @Test func unitViewLayoutMatchesHeader() {
        #expect(MemoryLayout<ScUnitView>.size == 40)
        #expect(MemoryLayout<ScUnitView>.stride == 40)
        #expect(MemoryLayout<ScUnitView>.alignment == 8)
        #expect(MemoryLayout<ScUnitView>.offset(of: \.x) == 8)
        #expect(MemoryLayout<ScUnitView>.offset(of: \.vx) == 20)
        #expect(MemoryLayout<ScUnitView>.offset(of: \.kind) == 32)
        #expect(MemoryLayout<ScUnitView>.offset(of: \.affiliation) == 34)
        #expect(MemoryLayout<ScUnitView>.offset(of: \.flags) == 35)
        #expect(MemoryLayout<ScUnitView>.offset(of: \.ems_power_w) == 36)
        #expect(MemoryLayout<ScFrame>.size == 24)
    }

    @Test func abiVersionMatchesHeader() {
        #expect(SimWorld.abiVersion == UInt32(SC_ABI_VERSION))
    }

    @Test func flagConstantsMatchHeader() {
        #expect(SimWorld.flagEmitting == 1 << 0)
        #expect(SimWorld.flagCommandNode == 1 << 1)
        #expect(SimWorld.flagDetected == 1 << 2)
        #expect(SimWorld.flagDetected == SC_FLAG_DETECTED)
        // Distinct bits, so they can be tested independently in `ScUnitView.flags`.
        let all = [SimWorld.flagEmitting, SimWorld.flagCommandNode, SimWorld.flagDetected]
        #expect(all.reduce(0, |) == 0b111)
        #expect(SimWorld.affiliationUnknown == 0)
    }

    @Test func emptyWorldHasOnlyTheCommandNode() throws {
        let world = try #require(SimWorld(seed: 1))
        #expect(world.withFrame { $0.count } == 1)
    }

    @Test(arguments: [0, 1, 1000] as [UInt32])
    func stepsAndFrameCountIsUnitsPlusOne(units: UInt32) throws {
        let world = try #require(SimWorld(seed: 3, dummyUnits: units))
        #expect(world.tick == 0)
        world.step(1)
        world.step(SimWorld.ticksPerSecond)
        #expect(world.tick == 1 + UInt64(SimWorld.ticksPerSecond))
        #expect(world.withFrame { $0.count } == Int(units) + 1)
    }

    @Test func exactlyOneCommandNodeAtOrigin() throws {
        let world = try #require(SimWorld(seed: 9, dummyUnits: 5000))
        world.step(30)
        let nodes = world.withFrame { units in
            units.filter { $0.flags & SimWorld.flagCommandNode != 0 }
        }
        try #require(nodes.count == 1)
        #expect(nodes[0].x == 0 && nodes[0].y == 0 && nodes[0].z == 0)
        #expect(nodes[0].affiliation == SimWorld.affiliationFriendly)
    }

    @Test func sameSeedGivesByteIdenticalFramesAfter300Ticks() throws {
        let a = try #require(SimWorld(seed: 42, dummyUnits: 10_000))
        let b = try #require(SimWorld(seed: 42, dummyUnits: 10_000))
        a.step(300)
        for _ in 0..<300 { b.step(1) }  // Same ticks, different batching.
        #expect(a.tick == 300 && b.tick == 300)
        let fa = frameBytes(a)
        #expect(fa.count == 10_001 * 40)
        #expect(fa == frameBytes(b))

        let c = try #require(SimWorld(seed: 43, dummyUnits: 10_000))
        c.step(300)
        #expect(fa != frameBytes(c))
    }

    @Test func unrepresentableUnitCountFails() {
        #expect(SimWorld(seed: 1, dummyUnits: .max) == nil)
    }

    @Test func withFrameRethrows() throws {
        struct Stop: Error {}
        let world = try #require(SimWorld(seed: 1))
        #expect(throws: Stop.self) { try world.withFrame { _ in throw Stop() } }
        // The world is still usable after the body threw.
        world.step(1)
        #expect(world.tick == 1)
    }
}

/// The Phase 1 bridge benchmark as a test, so it can run on an iOS simulator through
/// `xcodebuild test`. Off by default; enable with `SIMCORE_BENCH=1` (for xcodebuild,
/// `TEST_RUNNER_SIMCORE_BENCH=1`). Only meaningful in a Release build.
@Suite struct BridgeBenchmarkTests {
    @Test(.enabled(if: Environment.flag("SIMCORE_BENCH")))
    func bridgeBenchmark50k() {
        let report = BridgeBenchmark.run(units: 50_000, iterations: 300)
        print("SIMCORE_BENCH_BEGIN\n\(report.formatted())SIMCORE_BENCH_END")
        #expect(report.frameBytes == 50_001 * 40)
    }
}

private enum Environment {
    static func flag(_ name: String) -> Bool {
        ProcessInfo.processInfo.environment[name] == "1"
    }
}
