# Benchmarks

Release build (`lto = "fat"`, `codegen-units = 1`), Apple M6 Mac (macOS 27.0), rustc 1.99.1,
bevy_ecs 0.20.0, single-threaded executor. 50,000 dummy units plus the command node.

```sh
cargo run --release -p simcore-engine --example bench -- 50000
```

| Measurement | Result |
|---|---|
| `step(1)` (EMS duty cycle + movement) | ~0.055 ms median (~1.1 ns/unit) |
| `frame()` (rebuild 50,001 views) | ~0.085 ms median (~1.7 ns/unit) |
| Frame buffer | 2,000,040 B (1.91 MiB; 40 B x 50,001, exact capacity) |
| ECS world heap after `new_dummy` | 11,368,880 B (10.84 MiB, ~227 B/unit) |
| Heap allocations in 300 x (`step(1)` + `frame()`) | 0 |

Medians over 300 iterations after one warm-up step and frame, with hot caches. Memory comes from
a counting global allocator in the example, so it is exact heap bytes, not RSS. The ECS figure
includes table growth slack and per-component change ticks; raw component data is ~79 B/unit.

In the real-time loop (`examples/headless.rs`, 50,000 units) a tick plus frame cost ~1 ms,
because the thread sleeps between ticks and wakes with cold caches and a lower clock. That is the
better guide to per-tick cost in an app; the table above is the steady-state ceiling.
