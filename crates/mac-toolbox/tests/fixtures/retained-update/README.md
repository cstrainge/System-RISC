This private Finder runs `tests/retained_update_cases.rs` through the real public
Toolbox SDK. It launches the disk's normal QuickDraw Test application, checks
that both applications have distinct actual OS threads, and restores its own
window to the foreground.

The fixture commits a red scene, starts another BeginUpdate, and waits at empty,
half-drawn and fully drawn pending checkpoints before EndUpdate. A second update
replaces the committed split scene with yellow. Each checkpoint repeatedly calls
NextEvent and input-state services while the other application also polls events;
physical Space input advances the fixture only after capture. The guest verifies
that the background thread continues to receive timer slices.

`capture_system7_retained_update.py` packages only a private cloned boot disk,
transports Space input and records seventeen screenshots. The canonical host Rust
test checks exact pixels in the window's content interior, excluding the
standard definition's reserved scrollbar and grow strips. Pending scenes must
retain the previous committed image; EndUpdate publishes the complete new image.
Four additional checkpoints exercise a diagonal pattern at an unaligned window
origin, drawing appended outside BeginUpdate, temporary window occlusion, and
the complete scene exposed after closing the covering window. Repeated input
polling and cursor presentation must preserve pattern phase and current pixels.
Six desktop checkpoints hide the other application's window, commit cyan,
cancel a pending red replacement, commit magenta, then cover and reveal the
desktop. Rust checks both desktop pixels and the retained patterned window.
This is the native retained-rendering/preemption contract, not a claim that
classic immediate QuickDraw drawing had transaction semantics.

Run after `cargo xtask image`:

```sh
cargo test --locked -p mac-toolbox --test retained_update -- --ignored
```

The same test is included in `cargo xtask check`. The visible desktop is never
used by this transport.
