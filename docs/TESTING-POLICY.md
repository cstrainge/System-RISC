# Testing policy

This project validates the operating system **in the target runtime**. There is
no host-linked Rust test suite for target software, and none may be added.

## What is forbidden

- `tests/` directories in `crates/`, `drivers/` or `system/`.
- `[[test]]` targets and dev-dependencies that exist only to build one.
- Inline `#[cfg(test)]` modules, helpers, fields and imports in target software.
- Anything that links target code against host allocation, host trap entry,
  host scheduling or a substitute interrupt gate.

Those suites exercised shared source through a host gate that substituted the
production RV64 trap entry, MMU, interrupt, preemption, scheduler and allocator
paths. A passing run could not establish anything about the shipping machine.

`scripts/check_no_host_tests.py` enforces the rule and reports every offender.

## What is allowed

- `#[cfg(not(test))]`, `cfg!(test)` and any other use of the `test` cfg that
  guards production code. These are not test items; deleting them changes
  behaviour.
- `tools/*/tests/` — host tooling runs on the developer machine, not in the
  guest, so it keeps its own suites.
- `crates/mac-toolbox/tests/fixtures/` — standalone guest workspaces that the
  boot cases build and run (`pipes-probe`, `fonts-probe`). These are target
  applications, not host tests.
- `crates/mac-toolbox/tests/data/` — the disk images and captured corpora those
  cases mount.

## How to validate a change

```sh
cargo xtask boot-check --platform qemu_riscv
cargo xtask boot-check --platform qemu_arm64
```

The RISC-V run boots the built ROM and disk images in QEMU and reports one
result per case; the arm64 run must reach
`DESKTOP_READY: Finder and native Window Manager`. Debug assertions and
invariant checks live in the real code so the first violation is caught near
its cause, and release builds are exercised with the normal shipping
configuration.

A successful `boot-check` establishes only the cases it ran, on the artifacts
and configuration it ran. It does not establish broader compatibility, host
tool behaviour or hardware other than what the emulator models.
