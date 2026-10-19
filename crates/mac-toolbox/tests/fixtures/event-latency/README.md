# Native wait and dispatch probe

The private Finder enters only the public aggregate Toolbox SDK. Canonical
assertions live in `tests/event_latency_cases.rs`; the Python driver only builds,
boots, injects physical virtio input and records evidence. The standard OS image,
its ROM and System resources are hashed before and after the run.

Coverage includes mask preservation, exactly-once retrieval, an already queued
match, zero-timeout polling, real timeout expiry, pointer-motion null wakeup,
posted Null payloads, physical key down/up ordering, and input during a separate application's
three-second no-trap/no-yield CPU loop. The contention scene has three actual
application threads: this probe, the installed QuickDraw Test, and ProcessSpinner.

Two sets of 64 samples use the public nanosecond monotonic clock around posting
an application event and returning it through `wait_event`. The report retains
p50, p95, p99 and maximum. These observe SDK dispatch and scheduling overhead;
they do not measure host QMP input transport or the time to a presented display
frame. A one-millisecond goal is recorded, without making correctness depend on
a host-dependent timing threshold. A separately recorded process-switch count
checks that the waiting probe is scheduled at most eight times over a six-tick
wait, leaving margin around the two public queries while rejecting busy polling.

Run after building the current OS image:

```sh
CARGO_INCREMENTAL=0 cargo test --locked -p mac-toolbox --test event_latency -- --ignored
SYSTEM7_EVENT_LATENCY_WINDOWS=1 CARGO_INCREMENTAL=0 cargo test --locked -p mac-toolbox --test event_latency -- --ignored
```

Both transports use private copies. Windows capture listens on port4457; the
visible desktop at4447 is never targeted.

With an `SYSTEM7_EVENT_TRACE=1 cargo xtask image` build, the transport also
records 16 decoded physical-key delivery intervals. The intentionally masked
key is excluded because its one-second retention is asserted separately. Set
`SYSTEM7_STRICT_EVENT_LATENCY=1` to require all physical and queued samples below
1 ms; strict mode rejects a nontraced image rather than skipping physical timing.
The default gate checks correctness without imposing a host-dependent deadline.
