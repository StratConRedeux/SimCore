@_exported import SimCoreC

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

    deinit {
        sc_world_destroy(handle)
    }

    /// The ABI version of the linked engine (`SC_ABI_VERSION` when they match).
    public static var abiVersion: UInt32 { sc_abi_version() }

    // The header's `#define` constants import as `Int32` (or `UInt32` for `1u << n`); these
    // are the same values with the types of the fields and parameters they go with.
    public static let ticksPerSecond = UInt32(SC_TICKS_PER_SECOND)
    public static let flagEmitting = UInt8(SC_FLAG_EMITTING)
    public static let flagCommandNode = UInt8(SC_FLAG_COMMAND_NODE)
    public static let affiliationFriendly = UInt8(SC_AFFILIATION_FRIENDLY)
    public static let affiliationHostile = UInt8(SC_AFFILIATION_HOSTILE)
    public static let affiliationNeutral = UInt8(SC_AFFILIATION_NEUTRAL)

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
        return try body(UnsafeBufferPointer(start: frame.units, count: Int(frame.count)))
    }
}

/// Explicitly not Sendable: see the threading note on `SimWorld`.
@available(*, unavailable)
extension SimWorld: Sendable {}
