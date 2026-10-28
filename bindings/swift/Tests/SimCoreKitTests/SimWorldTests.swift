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

/// Contract revision 1.2: commands, status, link delay, cleanup, save and restore.
@Suite struct ContractRevision12Tests {
    @Test func contractLayoutsMatchHeader() {
        #expect(MemoryLayout<ScCommand>.size == 40)
        #expect(MemoryLayout<ScCommand>.alignment == 8)
        #expect(MemoryLayout<ScCommand>.offset(of: \.type) == 32)
        #expect(MemoryLayout<ScStatus>.size == 48)
        #expect(MemoryLayout<ScBytes>.size == 16)
    }

    @Test func commandsStatusAndLinkDelay() throws {
        let world = try #require(SimWorld(seed: 4, dummyUnits: 200))
        #expect(world.linkDelay(unit: 1) == 0)
        #expect(world.linkDelay(unit: 1 << 40) == nil)
        let id = world.command(1, .move(x: 30, y: 0, z: 0, speedMps: 10))
        #expect(id == 1)
        #expect(world.command(1, .move(x: 30, y: 0, z: 0, speedMps: 0)) == nil)
        #expect(world.command(1, .retarget(2)) == nil, "the command node is not a pursuer")
        #expect(world.command(1 << 40, .halt) == nil)
        #expect(world.status.pending_commands == 1)
        world.step(60)
        let s = world.status
        #expect(s.tick == 60 && s.pending_commands == 0 && s.unit_count == 201)
        world.despawnRemoved()
    }

    @Test func saveAndRestoreContinueIdentically() throws {
        let a = try #require(SimWorld(seed: 8, dummyUnits: 2000))
        a.step(45)
        a.command(1, .move(x: -50, y: 20, z: 0, speedMps: 5))
        let bytes = a.save()
        #expect(!bytes.isEmpty)
        let b = try #require(SimWorld(snapshot: bytes))
        #expect(b.save() == bytes, "canonical bytes")
        a.step(90)
        b.step(90)
        #expect(a.status.tick == b.status.tick)
        // Frame order is unspecified after a restore: compare by id.
        func sorted(_ w: SimWorld) -> [UInt8] {
            w.withFrame { units in
                units.sorted { $0.id < $1.id }.withUnsafeBytes { Array($0) }
            }
        }
        #expect(sorted(a) == sorted(b))

        var damaged = bytes
        damaged[damaged.count / 2] ^= 0x10
        #expect(SimWorld(snapshot: damaged) == nil)
        #expect(SimWorld(snapshot: Data()) == nil)
    }

    @Test func catchUpIsCapped() throws {
        let w = try #require(SimWorld(seed: 1))
        #expect(w.catchUp(elapsedTicks: 10, maxTicks: 100) == 10)
        #expect(w.catchUp(elapsedTicks: 1_000_000, maxTicks: 300) == 300)
        #expect(w.tick == 310)
    }
}

@Suite struct ContractRevision13Tests {
    @Test func setupLayoutsMatchHeader() {
        #expect(MemoryLayout<ScRules>.size == 48)
        #expect(MemoryLayout<ScOutcome>.size == 16)
        #expect(MemoryLayout<ScLinkRules>.size == 32)
        #expect(MemoryLayout<ScTerrain>.size == 40)
        #expect(MemoryLayout<ScUnitSpec>.size == 112)
        #expect(MemoryLayout<ScUnitSpec>.offset(of: \.options) == 55)
        #expect(MemoryLayout<ScUnitSpec>.offset(of: \.target_id) == 72)
        #expect(MemoryLayout<ScSensor>.size == 16)
    }

    @Test func aStrikeBuiltFromSwiftReachesItsTarget() throws {
        let world = try #require(SimWorld(seed: 3))
        var rules = ScRules()
        rules.detection_max_range_m = 20_000
        rules.sensor_base_range_m = 2_000
        rules.sensor_reference_power_w = 1
        rules.exposure_threshold_ticks = 2
        rules.exposure_cap_ticks = 10
        rules.exposure_decay_per_tick = 1
        var hit = ScOutcome()
        (hit.outcome_class, hit.target_kind, hit.chance) = (1, 4, 1)
        #expect(world.setRules(rules, outcomes: [hit]))
        #expect(!world.setRules(rules, outcomes: [hit, hit]), "repeated pair")
        var link = ScLinkRules()
        (link.base_delay_ticks, link.max_delay_ticks) = (3, 30)
        #expect(world.setLinkRules(link))
        #expect(world.linkDelay(unit: 1) == 3)
        var terrain = ScTerrain()
        (terrain.spacing_m, terrain.nx, terrain.ny, terrain.los_samples) = (100, 2, 1, 4)
        #expect(world.setTerrain(terrain, heights: [0, 0]))
        #expect(!world.setTerrain(terrain, heights: [0]))
        #expect(world.setTerrain(nil))

        var radar = ScUnitSpec()
        (radar.x, radar.erp_w, radar.kind) = (1_000, 50_000, 4)
        radar.affiliation = SimWorld.affiliationHostile
        radar.options = SimWorld.spawnEmitting
        let target = try #require(world.spawn(radar))
        var strike = ScUnitSpec()
        (strike.vx, strike.kind, strike.affiliation) = (1, 3, SimWorld.affiliationFriendly)
        strike.options = SimWorld.spawnPursuer
        (strike.target_id, strike.outcome_class) = (target, 1)
        (strike.speed_mps, strike.max_turn_rate_rad_s, strike.engagement_radius_m) = (100, 1, 10)
        var bad = strike
        bad.speed_mps = 0
        #expect(world.spawn(bad) == nil)
        #expect(world.spawn(strike) == 3)
        #expect(world.addSensor(unit: 1))
        #expect(!world.addSensor(unit: 99))

        world.step(5)
        #expect(world.status.hostile_detected == 1)
        world.step(200)
        #expect(world.status.unit_count == 1, "the strike removed the radar and itself")
        let restored = try #require(SimWorld(snapshot: world.save()))
        #expect(restored.linkDelay(unit: 1) == 3, "setup is saved with the world")
    }
}
