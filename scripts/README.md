# OS development harness

These are the canonical native System7 VM launchers, capture transports and
integration checkers. Run from the enclosing development workspace, for example:

```sh
python3 System7/scripts/system7_vm.py start
cargo test --manifest-path System7/Cargo.toml -p mac-toolbox --tests
cd System7 && cargo xtask check
```

The directory above this repository holds generated `build/`, `results/`,
`working/` and private `.toolchains/` and original reference artifacts. They are
intentionally outside the OS source repository. Host userspace tests own their
small independent/retained fixtures under `crates/mac-toolbox/tests/data`.
Full original-System visual comparisons additionally require the private
reference corpus and hfsutils/QEMU tools from the RE development environment.

The three `capture_system7_*` scripts collect inputs, serial results and frames;
canonical Rust tests in the aggregate Toolbox crate own new acceptance decisions.
Older Python visual checkers remain migration work recorded in TOOLBOX-PLAN.md.
