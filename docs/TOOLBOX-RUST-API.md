# Public Rust Toolbox API worklist

Verified on 2026-10-20 before the driver/module milestone.
Source review: workspace `toolbox-rust-api-audit-2026-10-20.md`, baseline `96461ba`.
The ABI remains the transport and classic compatibility contract. Native apps use
thin typed clients over the same resident services. Rebuild all apps together;
remove superseded native paths rather than preserving old project APIs.

- [x] Decoded events, modifiers and masks; Finder and shipped apps migrated.
- [x] Non-exhaustive typed errors, including lossless unknown status diagnostics;
      the facade preserves SDK errors and identities.
- [x] Distinct volume, catalog, process and menu identities.
- [x] Decoded information snapshots, including precise timestamps and object kinds.
- [x] Owned TextEdit editors, menus and alerts, with explicit borrowed references
      and cleanup that retains ownership on errors.
- [x] Type codes, scrollbar parts, named drawing operations, memory states/errors,
      private WindowSpec builders and automatic-clock mutation convenience APIs.
- [x] Document and verify the native threading contract.

Event polling and waiting consistently return a decoded Event. Idle is an
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

## Verified native SDK milestone — ABI 7

Native volume/catalog/process/thread/menu identities and resource scopes are
distinct types. TypeCode preserves FourCC bytes; information snapshots expose
checked UTF-8, Timestamp values, object kinds, boolean state and typed flags
instead of request layouts. Explicit wire access lives under mac_toolbox::abi.
PNG failures retain both a typed service error and a decoder-specific ErrorKind.
Drawing operations, compact bitmap font selectors, scrollbar parts and memory
states/errors use named vocabularies. Ordinary filesystem mutations and data-file
creation own the clock; `_at` variants preserve supplied timestamps.

TextEdit editors, menus and alerts have owners and borrowed references. Shipped
apps retain those owners. Explicit cleanup retains ownership on service errors;
Drop makes a non-panicking best effort. Native and Pascal TextEdit calls validate
the same shared handle table. TextEdit and native menu objects use central,
atomic generation histories across app-local contexts, with slot retirement.
Memory's wire format and existing process-local policy are unchanged. MenuId
names a semantic menu property; MenuHandle identifies a live object incarnation.

All six shipped apps, fixtures, reference tools and tests build against ABI 7.
Native definition manifests name an `artifact`, resolved beside the actual input
ELF in its selected target/profile; Cargo dependency tracking follows that same
profile. Private fixture caches are profile-local. The private ABI-7 guest passed
while the regular profile still contained ABI-6 definitions, proving that those
stale artifacts were no longer mixed into the check image.

The complete cargo xtask check gate passed: 137 host SDK tests, 23 private
graphical tests, an allocated fork crossing 4 GiB, 17 ROM boot/rejection cases,
compiled dependency/symbol boundaries and the original visual comparisons.
The normal optimized installation was rebuilt afterward. Strict SDK and shipped
RV64 app Clippy checks pass; all 13 RV64 fixtures and the two optional window
fixture modes compile. Obsolete generated images were pruned after checks.

This closes the cited Rust API audit, not the remaining Toolbox roadmap. Original
68k execution remains incomplete; TrueType/synthetic-style comparisons retain
their recorded pixel differences. Event/cursor regression gates pass, but the
existing latency report does not enforce its 1 ms target or measure complete
host-input-to-present latency. Private evidence is in results/system7 outside
this repository, including toolbox-api-full-check.log and toolchain-checks.json.
