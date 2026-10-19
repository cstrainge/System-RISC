# Window Manager Rust bindings

This crate exposes safe Rust requests to resident System's Window Manager.
It owns no window state, compositor, tracking logic or standard WDEF code.

```rust,ignore
use toolbox_windows::{Rect, WindowSpec};

let window = system.windows().create(
    WindowSpec::new(Rect::new(40, 60, 360, 280), "My window")
        .minimum(120, 80)
        .procedure(0),
)?;
system.windows().select(window)?;
```

`Windows` is constructed from a validated `mac_traps::Client`. Window IDs are
typed, errors use `Result`, and requests cross the loader-supplied trap gate.
The `definition` module reexports protocol types for independently loaded WDEFs.

See [the resident manager notes](../../system/src/toolbox/windows/README.md)
for implementation, fidelity and current limitations, and
[ARCHITECTURE.md](../../ARCHITECTURE.md) for the service boundary.
