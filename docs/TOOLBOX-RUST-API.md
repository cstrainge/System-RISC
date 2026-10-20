# Public Rust Toolbox API worklist

Current priority, before the driver/module milestone, per the user on 2026-10-20.
Source review: workspace `toolbox-rust-api-audit-2026-10-20.md`, baseline `96461ba`.
The ABI remains the transport and classic compatibility contract. Native apps use
thin typed clients over the same resident services. Rebuild all apps together;
remove superseded native paths rather than preserving old project APIs.

- [x] Decoded events, modifiers and masks; Finder and shipped apps migrated.
- [ ] Non-exhaustive typed errors, including lossless unknown status diagnostics;
      the facade preserves SDK errors and identities.
- [ ] Distinct volume, catalog, process and menu identities.
- [ ] Decoded information snapshots, including precise timestamps and object kinds.
- [ ] Owned TextEdit editors, menus and alerts, with explicit borrowed references
      and cleanup that retains ownership on errors.
- [ ] Type codes, scrollbar parts, named drawing operations, memory states/errors,
      private WindowSpec builders and automatic-clock mutation convenience APIs.
- [ ] Document and verify the native threading contract.

Event polling and waiting will consistently return a decoded Event. Idle is an
explicit variant retaining the physical input snapshot (and an opaque classic
null-event tag), so pointer-motion wakes and caret work require no extra trap.
Peek remains Option because queue presence is observable, including posted idle
records. Original OS queues retain their full records under `events::classic`.

Clients and owners are deliberately thread-mobile. Shared resident managers own
synchronization; service entry derives trusted caller identities. Mobility is
not an ownership transfer or permission to operate another application's objects.
Thread-specific operations still return WrongThread where required; there is
no UI worker-thread creation API yet. Copy manager views grant routing access,
not mutable references to resident state. Existing shared-memory concurrency
acceptance exercises simultaneous calls and serialized object access.

The events/error slice passed 132 host SDK tests (25 hardware/large-image tests
excluded by their existing ignore gates), all 13 RV64 fixture compile checks,
and seven selected private QEMU tests: aggregate conformance (including ten typed
event variants), physical input, event latency, menus, processes, typing and busy
cursor. The host adapter explicitly rejects unsupported waits; RV64 exercises
real waits and stale-window failures. This does not sign off the remaining worklist.

Verification belongs in crates/mac-toolbox/tests, through real resident services.
Rebuild the optimized installation, use private QEMU acceptance, retain cursor/
event performance gates and prune generated disks at milestone boundaries.

After every Toolbox audit fix and application migration is verified, commit and
push that complete milestone to the configured GitHub remote (explicit user
authorization, 2026-10-20). Then implement the driver/module and filesystem-class
foundation, the native host-directory driver pair and StuffIt archive driver.
Finder double-click on a StuffIt file mounts its filesystem. Keep both mounts
loadable through real DRVR/DABI contracts; do not replace them with built-in
shortcuts. Driver packages stay in Drivers; extensions stay in Extensions.
