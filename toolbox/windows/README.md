# Window Manager Rust bindings

This crate exposes safe Rust requests to resident System's Window Manager.
Window state, compositing, tracking and standard WDEF code remain in System.

```rust,ignore
use toolbox_windows::{Rect, WindowSpec};

let mut window = system.windows().create_owned(
    WindowSpec::new(Rect::new(40, 60, 360, 280), "My window")
        .minimum(120, 80)
        .procedure(0),
)?;
window.select()?;
system.quickdraw().graphics(window.id());
// Scope exit (including an early return through ?) closes the window.
// Explicit close reports errors and retains ownership when it fails.
window.close()?;
```

`Windows` is constructed from a validated `mac_traps::Client`. Window IDs are
typed, errors use `Result`, and requests cross the loader-supplied trap gate.
`Window` uniquely owns the lifetime and implements `Drop`; it is neither `Copy`
nor `Clone`. `id()` supplies a non-owning event/drawing identity. `into_id()`
explicitly transfers cleanup responsibility to the caller. Existing ID-based
`create`/`close` remain available for applications with manual lifecycle code.
An already closed window is harmless to destroy, and its identity never changes
to the special front-window ID0. Destructors do not panic or spin on contention;
use explicit `close(&mut self)` when its result matters. Drop remains best-effort
on a temporarily busy service, as documented in [THREADING.md](../../THREADING.md).
The `definition` module reexports protocol types for independently loaded WDEFs.

See [the resident manager notes](../../system/src/toolbox/windows/README.md)
for implementation, fidelity and current limitations, and
[ARCHITECTURE.md](../../ARCHITECTURE.md) for the service boundary.
