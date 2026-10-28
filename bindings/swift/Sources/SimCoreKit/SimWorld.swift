import Foundation
@_exported import SimCoreC

/// What a command tells a friendly unit to do. Positions are metres relative to the command
/// node at the moment the command is issued, the same frame as `ScUnitView`.
public enum SimOrder: Equatable, Sendable {
    /// Travel straight to `(x, y, z)` at `speedMps`, then stop. Not for pursuers.
    case move(x: Float, y: Float, z: Float, speedMps: Float)
    /// Cancel any move and stop. Not for pursuers.
    case halt
    /// A pursuer pursues another unit instead.
    case retarget(UInt64)
    /// Follow the duty cycle (or emit), or stay silent.
    case emissions(radiate: Bool)
}

/// One simulated world: a thin owner of an `ScWorld *` from the SimCore C API.
///
/// Threading: the FFI contract allows calls on one world from one thread at a time, so
/// `SimWorld` is deliberately **not** `Sendable`. Create and use it on a single thread, actor or
/// serial queue; Swift 6 strict concurrency then stops it from crossing isolation domains.
public final class SimWorld {
    private let handle: OpaquePointer
    /// True while a `withFrame` body runs. Engine frame memory is only valid until the next
    /// mutating call, so mutating from inside the body is a programming error.
    private var readingFrame = false

    /// An empty world containing only the player's command node. `nil` if the engine fails.
    public init?(seed: UInt64) {
        guard let h = sc_world_create(seed) else { return nil }
        handle = h
    }

    /// A world with the command node plus `dummyUnits` generated units. `nil` if the engine
    /// fails (including `dummyUnits == UInt32.max`, which the frame count could not represent).
    public init?(seed: UInt64, dummyUnits: UInt32) {
        guard let h = sc_world_create_dummy(seed, dummyUnits) else { return nil }
        handle = h
    }

    /// A world restored from `save()` bytes, continuing exactly as the saved one would have.
    /// `nil` if the bytes are not a snapshot this engine reads, or are damaged.
    public init?(snapshot: Data) {
        let h = snapshot.withUnsafeBytes { raw in
            sc_world_restore(raw.bindMemory(to: UInt8.self).baseAddress, UInt64(raw.count))
        }
        guard let h else { return nil }
        handle = h
    }

    deinit {
        sc_world_destroy(handle)
    }

    /// The ABI version of the linked engine (`SC_ABI_VERSION` when they match).
    public static var abiVersion: UInt32 { sc_abi_version() }

    // The header's typed constants (revision 1.1) already import with the types of the fields
    // and parameters they go with; these are Swift-style names for them.
    public static let ticksPerSecond: UInt32 = SC_TICKS_PER_SECOND
    public static let flagEmitting: UInt8 = SC_FLAG_EMITTING
    public static let flagCommandNode: UInt8 = SC_FLAG_COMMAND_NODE
    public static let flagDetected: UInt8 = SC_FLAG_DETECTED
    public static let affiliationUnknown: UInt8 = SC_AFFILIATION_UNKNOWN
    public static let affiliationFriendly: UInt8 = SC_AFFILIATION_FRIENDLY
    public static let affiliationHostile: UInt8 = SC_AFFILIATION_HOSTILE
    public static let affiliationNeutral: UInt8 = SC_AFFILIATION_NEUTRAL

    /// Advances `ticks` fixed steps of `1 / SC_TICKS_PER_SECOND` seconds.
    public func step(_ ticks: UInt32 = 1) {
        precondition(!readingFrame, "SimWorld.step called inside withFrame")
        sc_world_step(handle, ticks)
    }

    /// Ticks run since creation.
    public var tick: UInt64 { sc_world_tick(handle) }

    /// Builds the frame relative to the command node and passes the engine's buffer to `body`,
    /// with no copy.
    ///
    /// The buffer is valid only during `body`: don't let it escape, and don't call `step` or
    /// `withFrame` on this world from inside it. Copy what you need to keep.
    public func withFrame<R>(
        _ body: (UnsafeBufferPointer<ScUnitView>) throws -> R
    ) rethrows -> R {
        precondition(!readingFrame, "SimWorld.withFrame is not reentrant")
        readingFrame = true
        defer { readingFrame = false }
        let frame = sc_world_frame(handle)
        // `units` is NULL when the frame is empty or the call failed; both read as no units.
        guard let units = frame.units else {
            return try body(UnsafeBufferPointer(start: nil, count: 0))
        }
        return try body(UnsafeBufferPointer(start: units, count: Int(frame.count)))
    }
}

// MARK: - Contract revision 1.2: commands, status, cleanup, save and restore

extension SimWorld {
    /// Queues a command for `unit`, delivered after the link delay. Returns the command id, or
    /// `nil` if the engine rejects it (unknown or non-friendly unit, wrong unit type, invalid
    /// values).
    @discardableResult
    public func command(_ unit: UInt64, _ order: SimOrder) -> UInt64? {
        var c = ScCommand()
        c.unit_id = unit
        switch order {
        case let .move(x, y, z, speed):
            c.type = SC_COMMAND_MOVE
            (c.x, c.y, c.z, c.speed_mps) = (x, y, z, speed)
        case .halt:
            c.type = SC_COMMAND_HALT
        case let .retarget(target):
            c.type = SC_COMMAND_RETARGET
            c.target_id = target
        case let .emissions(radiate):
            c.type = SC_COMMAND_EMISSIONS
            c.radiate = radiate ? 1 : 0
        }
        let id = sc_world_command(handle, &c)
        return id == 0 ? nil : id
    }

    /// Ticks a command to `unit` issued now would take to arrive; `nil` if it does not exist.
    public func linkDelay(unit: UInt64) -> UInt32? {
        let d = sc_world_link_delay(handle, unit)
        return d == UInt32.max ? nil : d
    }

    /// Counts and the command node's link delay, for the HUD.
    public var status: ScStatus { sc_world_status(handle) }

    /// Releases the storage of removed units. Call it at natural pauses.
    public func despawnRemoved() {
        precondition(!readingFrame, "SimWorld.despawnRemoved called inside withFrame")
        sc_world_despawn_removed(handle)
    }

    /// The whole world as opaque bytes for `init?(snapshot:)`. Also releases removed units'
    /// storage. Empty only if the engine fails.
    public func save() -> Data {
        precondition(!readingFrame, "SimWorld.save called inside withFrame")
        let b = sc_world_save(handle)
        guard let data = b.data, b.len > 0 else { return Data() }
        return Data(bytes: data, count: Int(b.len))
    }

    /// Catches up after time away: runs `min(elapsedTicks, maxTicks)` ticks and returns how
    /// many it ran. The cap keeps a resume within a launch's time budget; what happens to the
    /// rest of the time away is the caller's rule.
    @discardableResult
    public func catchUp(elapsedTicks: UInt64, maxTicks: UInt32) -> UInt32 {
        let n = UInt32(min(elapsedTicks, UInt64(maxTicks)))
        step(n)
        return n
    }
}

/// Explicitly not Sendable: see the threading note on `SimWorld`.
@available(*, unavailable)
extension SimWorld: Sendable {}
