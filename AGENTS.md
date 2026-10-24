# System RISC development

This repository contains only the OS project. Keep the Rust workspace under this
root and the public Toolbox facade in `crates/mac-toolbox`. Finder depends only
on that facade at runtime. Resident implementations belong in System's internal
Toolbox modules; WDEF/CDEF and other definition resources own their unique code.

- No backward compatibility with earlier System RISC builds is required during
  this unreleased phase. Break internal APIs, ABIs, resource/native disk formats
  and preference schemas directly; update all producers, consumers, tools and
  tests together and rebuild the whole system. Regenerate generated media and
  remove superseded implementations. Do not add compatibility shims, dual
  decoders, fallbacks or migrations solely to preserve old project artifacts.
  Keep validation and clear rebuild/recreate errors for incompatible artifacts.
  Original Macintosh compatibility and external-format support remain explicit
  product requirements; this policy does not remove them.
- Keep the native development ABI at version 1 until the first proper release.
  Rebuild System, all apps, drivers, definitions and fixtures together whenever
  a contract changes; do not increment it for unreleased internal changes.
- Commit completed, verified milestones with descriptive messages. Do not push
  or publish without an explicit request. Keep generated targets, ROMs, disks,
  private working captures, and unrelated Strange Forth code outside this repo.
- Prune obsolete generated HD images at check boundaries, including on failure.
  Include HFS and other raw disk formats; retain only current build/live VM disks
  and original reference media. Reports and screenshots do not require old disks.
- Tests build their installation under a separate build/media root from the
  regular desktop. Propagate SYSTEM7_BUILD_DIR through all builders, guest
  launchers and acceptance readers; never hardcode the normal installation in
  a check. Native guest builds use the optimized release profile, including for
  the regular launcher; a host debug xtask must not imply a debug guest.
- P15's native HFS-B boot/default-image cutover is verified. Preserve its shared
  read/write core, resident/ROM boot, bounded resource-loading and public-SDK
  acceptance gates; host-only verification is insufficient for storage changes.
  Existing no_std apps remain the initial launch gate. Follow docs/HFS-B.md and
  docs/CORE-TOOLBOX-ROADMAP.md for explicit budgets and remaining filesystem APIs.
- HFS-B versions are whole integers (`1`, `2`, `3`), each implying one complete
  disk and Unicode contract. Changing Unicode requires a new filesystem version
  and rebuilding all affected file/name/index representations with collision
  checks; the Unicode header field is validation, not an independent selector.
- P15 and the public Rust Toolbox API audit are verified
  (see `docs/TOOLBOX-RUST-API.md`). The current sequence is the class-neutral
  driver/module interface, exercised by
  StuffIt volumes and the vhost filesystem/block-device pair, then the virtual
  host clipboard extension. Bring in the necessary P6 loader/service machinery
  for these milestones; input/network/display extraction and std migration follow
  this initial storage/extension validation sequence.
- Install native driver packages in the blessed `System Folder/Drivers` directory
  and System extensions in `System Folder/Extensions`. Shared module machinery
  does not merge their discovery paths or package/entry contracts.
- Canonical new Toolbox compatibility assertions belong in
  `crates/mac-toolbox/tests` and exercise the public SDK through real services.
  Capture scripts may collect evidence; Rust owns new acceptance decisions.
  Use original System behavior and code to resolve ambiguity; retain provenance.
- New Rust apps get native functionality through Rust interfaces. Legacy
  Pascal/MacRoman conversion is exposed through legacy traps, backed by the
  same services. Keep date APIs compatible with2026 and checked epoch conversion.
- Every future app runs in its own actual OS thread. Legacy cooperative contexts
  and globals are virtualized within its application context. Keep shared state
  synchronized, mutable ownership explicit, and unsafe code at narrow boundaries.
- Keep the native runtime suitable for a future Rust std platform layer; Toolbox
  clients remain usable without std. Keep CPU-specific code in machine adapters:
  RISC-V is current, x86 is a future target. See THREADING.md for unmet gates.
- P8 makes std the default for every native userspace application, including
  Finder and apps/*. Build/startup plumbing belongs to the platform, not app
  no_std/no_main/entry/panic boilerplate. The platform default std panic hook
  calls the System-owned bomb dialog without app opt-in. Migrate apps only with
  verified target/sysroot/runtime support; retain no_std for kernel, minimal
  ABI/transport, Toolbox bindings and independently loaded low-level modules.
- Rebuilding images preserves saved Preferences and Documents through the shared
  HFS-B lossless export/import tool. Keep user-state exports outside generated
  media cleanup. Before replacing a writable VM disk, export its latest saved
  files and validate a staged restored disk; never read a live guest through
  offline tools or discard files after a preservation error.
- All system preferences use versioned JSON with typed access and safe concurrent
  transactions. Persistence requires writable storage; do not fake success.
- Every resident Toolbox manager must issue and validate object IDs through
  `system/src/toolbox/handles.rs` (the resident facade over the shared
  `crates/mac-object-table` utility); no manager may introduce its own ID scheme.
  Migrate existing ID storage when touching that manager. Preserve generation
  history across clears and retire exhausted slots; register disjoint service
  namespaces centrally and keep ownership checks separate. Keep validation in
  resident storage for both native Rust and legacy trap callers.
- Generate application vers resources from Cargo's resolved package version.
- Apps use managed Control Manager objects for standard buttons, checkboxes,
  radios and scrollbars, with CDEF-owned drawing/geometry and resident tracking,
  highlighting/cancellation. Use managed scroll boxes for compound views; do not
  duplicate control hit testing or press/release loops inside applications.
- Give every CDEF a descriptive control-family resource name and build artifact
  (for example, `CDEF_PROGRESS_BAR` / “Progress Bar”). Keep package and source
  names informative too; numeric resource IDs alone do not identify a control.
- Repainting is bounded by the changed object: cursor, control, or window area.
  Accumulate exact damage, preserve occlusion and clipping, and leave unrelated
  pixels and drawing work untouched. Movement damages both old and new bounds;
  a local update must never implicitly invalidate the desktop or parent window.

Performance bar: the code must not do unnecessary work. Full recomputation is the last resort, not the default: prefer bounded invalidation over full repaints, early-out on no-change, cache what doesn't move, and never do per-event work when per-change work will do. "Input happened" is not "pixels changed." When in doubt, the Cursor Test's responsiveness and the present() trace timings are the benchmarks — the mouse is the profiler.

- Every workflow test includes relevant unhappy paths and asserts informative
  error messages, not just status codes. Preserve causes through driver, resident
  service and app boundaries. Ordinary errors must release owned protocol state
  and leave the UI immediately usable; verify cleanup, retry and cancellation.
  If rollback itself fails, quarantine the affected resource and report recovery
  honestly. Contain application panics; Finder failure is recoverable by System
  cleanup and restart. A resident System panic shows diagnostic details and a
  Restart button through a minimal panic-safe input/drawing path; it does not
  attempt to continue running normal services.
- The memory/app/resource first cut was accepted on 2026-10-23. Follow
  docs/MEMORY-APP-RESOURCE-MODEL.md and its staged migration. Shared handle
  ledgers use OwnerRef with the explicit native architecture invocation gate;
  never bypass its private lock boundary. The physical buddy pool remains
  disconnected: establish its IRQ/preemption discipline before it serves traps
  or interrupts. Host-thread concurrency tests alone are not that proof.
- Use the supplied rustfmt.toml. Run checks appropriate to the change and report
  actual coverage and remaining gaps. A passing subset is not full Toolbox or
  original 68k application compatibility.
- Do not retain dead code or superseded implementations. Remove unused
  placeholders instead of suppressing dead-code warnings; introduce executable
  pieces with their consumers as the staged model becomes resident.

## Resident panic discipline

Ruling from Chloe, 2026-10-24: resident code must not use `.expect` on cleanup,
reap or paint paths. Cleanup/reap failures return the unique ticket and typed
cause to the walk's quarantine; they never panic. Paint skips a frame when its
resource owner has dropped an image; a missing image must not kill the System.
A preflighted lease must stay pinned across its use, or absence must be handled
fallibly. Preflight followed by `.expect` without a held lease is a defect.

`.expect` is reserved for structural invariants on non-failure paths, such as
scheduler bootstrap threads, bundled assets and validated parses. Every such
message must name the invariant. Do not replace forbidden `.expect` calls with
`unwrap`, `panic!`, or silent loss of cleanup tickets or protocol effects.
