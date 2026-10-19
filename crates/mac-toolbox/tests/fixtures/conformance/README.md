# Graphical userspace conformance application

This private Finder application imports the aggregate `mac-toolbox` SDK and canonical Rust tests from `tests/conformance`. It runs `run_all`, real HFS date checks against `dates-2026.hfs`, and the shared/graphical legacy Pascal–native UTF8 boundary checks. Only a successful return from every assertion emits `TOOLBOX_USERSPACE_PASS`; errors and panics emit a bounded diagnostic beginning `TOOLBOX_USERSPACE_FAIL`.

`capture_system7_conformance.py` builds/packages this application and replaces Finder only on a cloned disk. It boots the real ROM and graphical System with read-only storage and retains serial/artifact hashes. Python does not decide compatibility results. The aggregate ignored test `userspace_graphical.rs` asserts the complete guest marker, actual ABI4/CDEF load evidence, and unchanged original artifacts. `cargo xtask check` runs it after the image is built.

Run manually after `cargo xtask image`:

```sh
cargo test -p mac-toolbox --test userspace_graphical -- --ignored --nocapture
```

The older nongraphical ABI smoke application runs `conformance::run_core` only. Its smaller service table cannot establish control/font/resource/window-string conformance. See `results/system7/userspace-conformance-checks.json` for the graphical run and `tests/data/control-scope-ledger.json` for the bounded original control coverage and remaining TrackControl gaps.
