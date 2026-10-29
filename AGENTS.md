# System RISC development

This repository contains only the OS project. Keep the Rust workspace under this
root and the public Toolbox facade in `crates/mac-toolbox`. Finder depends only
on that facade at runtime. Resident implementations belong in System's internal
Toolbox modules; WDEF/CDEF and other definition resources own their unique code.

## Current work order — 2026-10-28

The user explicitly resumed the memory work to unblock dependent features.
Defer the Rust-test removal, whole-code audit and new validation-framework design
until after this work. Existing in-VM checks may be used for their specific
scenarios; host passes are not OS acceptance. The permanent evidence, production
architecture and instrumentation guardrails below still apply. Keep TE2/common
control migration deferred. Notify the user when an actual executable module
worker backend meets its stated gates; do not automatically resume the extension
agent or claim production clipboard readiness. The current handoff is scoped to
native broker delivery/completion and worker execution, cancellation, rollback
and actual reclamation. Full Save/Save As exhaustion and production Finder OOM
restart remain separate OS qualification. After publishing the Clipboard
development prerequisites, prioritize the kernel SMP safety gate; keep additional
harts parked until that gate passes.

## Verification failure and recovery order — 2026-10-27

This instruction supersedes earlier testing rules and claims of verification.
Treat the codebase as **unvalidated**. Previous Rust test pass counts do not
establish correctness of the shipping OS. Unsupported assurances are false
correctness claims; users may depend on them when making engineering decisions.

### Permanent agent discipline

These are decision rules for every future change, not only the cleanup:

- Before saying **tested**, **verified**, **working** or **complete**, identify
  the exact artifact/configuration, where it ran, the production path exercised,
  the observed result and what remains unverified. If those facts are missing,
  do not make the claim.
- Never treat shared source, a public-looking API, a passing host suite or a
  familiar test name as proof of equivalent target execution. Trace what
  actually ran. If a substitution removes the property being checked, its
  result cannot establish that property.
- Never add production indirection, exports, alternate implementations or
  configuration branches merely to make a harness run. Challenge the harness
  design first. Keep production-purpose abstractions and safety boundaries.
- Never replace missing runtime evidence with pass counts, compilation success,
  model reasoning, convenient assumptions or optimistic wording. A plausible
  implementation is still unverified.
- Treat allocation, interrupt state, preemption, synchronization, timing and
  cleanup behavior as correctness requirements. Investigate configuration
  differences before relying on a result; do not wave them away as equivalent.
- Give the user the limitations before they depend on the conclusion. Correct
  unsupported claims explicitly. Speed, token/time pressure and a desire to
  announce progress do not justify weaker evidence or hidden shortcuts.

### Findings

- Much of the suite compiled host-configured services with `std`, host
  allocation/synchronization, thread-local sessions and a replacement trap gate.
  In the OS repository, `system/src/testing/mod.rs::with_services` supplies
  `host_gate`. This bypasses production RV64 trap entry, MMU, interrupt,
  preemption, scheduler and allocator paths. Exercising shared source through
  this setup cannot qualify those production paths.
- Host testability also affected production structure. The System imports
  definition `main.rs` files into host test-support modules, while the CDEF
  scaffold forwards through a named entry, family adapter and binding before
  the author handler. Audit this coupling and justify every production layer.
  The observed source structure does not establish machine-code timing or
  overhead; measure those properties before making claims.
- Some retained QEMU runs execute real guest code. Their evidence applies only
  to the exact artifacts, paths and scenarios exercised. It does not establish
  broader coverage or rehabilitate the host suite.
- Runtime configuration and timing are part of correctness. Differences in
  allocation, interrupt masking, preemption, call paths or failure handling can
  hide intermittent failures and deadlocks. Compilation, boot success, screenshots,
  pass markers and a large pass count are insufficient on their own.

### Required work on explicit resumption

Development remains paused until the user explicitly resumes it. Editing these
instructions does not resume implementation, validation runs or extension work.

1. **First remove ALL Rust tests and test-only plumbing.** Include unit,
   integration and documentation tests; test-only modules, features, dependencies,
   fixtures and runners; and exports or forwarding layers whose only purpose is
   the removed harness. Trace references and preserve genuine production
   functionality, API documentation, validation and safety checks. Do not retain
   a preferred subset or silently introduce a replacement Rust test framework.
2. **Then audit the ENTIRE codebase.** Cover shared crates, kernel/resident
   services, Toolbox and driver SDKs, CDEF/WDEF and other definitions, drivers,
   extensions, applications, host tools and build paths. Look for alternate
   implementations/dispatch, host-only assumptions, direct inclusion of guest
   entry sources, redundant adapters/call chains, test-only API leakage,
   allocation/lifetime mistakes, and interrupt/scheduling/timing assumptions.
   Distinguish legitimate platform/tool boundaries from harness accommodations.
   Record each finding, its production rationale or defect, remediation and
   remaining uncertainty.
3. After removal and audit, implement the user-selected native test applications,
   test kernel and production invariant checks described below. Agree on remaining
   execution/reporting details before adding machinery. Do not carry forward old
   pass counts or completion labels as acceptance. Memory/allocator, common-control
   and extension feature work remains behind this recovery order.

### Native diagnostics and stress validation

The user has selected native test applications and a test kernel for future
validation. This is not permission to resume now or retain the old Rust suite.

- Build diagnostic/stress applications as real target applications using the
  public SDK, normal packaging/loading and actual traps. A dedicated test kernel
  must exercise the production implementation and real architecture/allocator
  paths. Do not create another kernel implementation or host service substitute.
- Put debug assertions and invariant checks in the real code. Check ownership,
  generations, accounting, pool structure, lifecycle transitions and balanced
  protocol state where those properties change, so the first violation is
  detected near its cause. Give failures useful, bounded context.
- Boundary validation and checks needed for safe release behavior stay active
  in the shipping configuration. `debug_assert!` disappears when disabled;
  it cannot be the only enforcement of a safety requirement. Exercise optimized
  builds with invariant checking and the normal shipping configuration, recording
  instrumentation/configuration differences and their limits.
- Preserve typed handling of malformed requests, stale/foreign objects, budget
  refusal and ordinary failures. Assertions must not replace recovery logic or
  evade cleanup/reap/paint panic discipline. Retain cleanup tickets and quarantine
  appropriately. Genuine unrecoverable System corruption follows the declared
  terminal diagnostic contract; do not claim arbitrary fault recovery.
- Keep hot-path checks bounded, preferably local and allocation-free, using
  existing synchronization. Do not introduce global check/log locks, extra
  fences, interrupts-off intervals, I/O or scheduling changes merely to observe
  an invariant. Expensive whole-structure checks need explicit diagnostic or
  quiescent execution rather than per-event scans.
- Native stress applications must hammer the real kernel with sustained,
  overlapping workloads and multiple real applications/threads: allocation and
  disposal, handle/resource reuse, launch/quit, file I/O and cancellation, UI/event
  traffic, pressure and failure paths. Include stale/foreign requests, interrupted
  operations and subsequent reuse. Honor actual configured concurrency support;
  a single-hart run does not establish SMP correctness.
- Avoid serializing the workload through a host acknowledgment after each
  operation or through test-only global barriers. Serial output, structured
  result encoding, synchronous transport and added synchronization can perturb
  timing, order execution and hide races. Use minimal instrumentation, bounded
  preallocated failure records and deferred/batched summaries where appropriate.
  Even memory tracing has cost; compare relevant behavior with tracing disabled.
- Retain workload/configuration provenance and reproducible seeds where used.
  Capture the first failure before later cleanup obscures it. Measure timing in
  the target runtime, report observer overhead and distinguish what was exercised
  from what is proven. Successful stress runs do not prove absence of races.
- Run destructive and stress scenarios in private guest environments, preserving
  the user's live VM and data. The authorized future guest applications/kernel
  are not a replacement host Rust unit/integration/doctest framework.

### Production design, evidence and reporting

- Production architecture serves the OS contract. Validation machinery must
  adapt to that architecture. Every retained abstraction must have a production
  purpose. Preserve the safe typed definition-author API and one narrow audited
  unsafe ABI boundary; removing wrappers must not push raw pointer/wire handling
  back into application or definition authors.
- OS acceptance must exercise the actual target artifacts and relevant shipping
  build configuration in the target runtime. For the current kernel and low-level
  modules that includes their real `no_std` implementation, allocator, trap,
  resource loading/relocation, interrupt and scheduler paths. This does not
  cancel the separate plan for application `std` support.
- An external host harness may drive QEMU and collect evidence. It cannot replace
  guest services with host-linked implementations and claim equivalent coverage.
  Agree on new validation machinery before adding it; the removal directive
  does not authorize another Rust testing framework.
- Label instrumented or fault-injection builds and their differences from the
  shipping configuration. They require production-path evidence for the property
  claimed. Preserve artifact/configuration provenance, observations, failure and
  cleanup behavior, and the limits of each result. Timing claims require measured
  target-runtime evidence; QEMU evidence alone does not qualify other hardware.
- Never report a feature, synchronization guarantee, failure path, performance
  property or milestone as verified without evidence supporting that exact
  claim. Report missing coverage as **unverified**. Build, format and static
  checks establish only their stated properties. Correct or withdraw unsupported
  completion claims; preserve historical evidence without presenting it as proof.
- Keep reports in root `status.txt` and coordinate through root `comms.txt`.
  Clipboard development readiness requires the recorded native broker/worker
  evidence and exact published starting commit. Do not resume the external agent
  automatically or equate those prerequisites, heap capacity, retained account
  authority or transport demos with production clipboard sharing or SMP safety.

## Architecture, runtime and project rules

- No backward compatibility with earlier System RISC builds is required during
  this unreleased phase. Break internal APIs, ABIs, resource/native disk formats
  and preference schemas directly; update all producers, consumers, tools and
  validation paths together and rebuild the whole system. Regenerate generated
  media and remove superseded implementations. Do not add compatibility shims, dual
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
- Remove finished auxiliary worktrees and merged task branches after checking
  clean status, ancestry and user files. Never force-remove dirty, unmerged or
  active work; retain the current worktree until its remaining work is merged.
- Prune obsolete generated HD images at check boundaries, including on failure.
  Include HFS and other raw disk formats; retain only current build/live VM disks
  and original reference media. Reports and screenshots do not require old disks.
- Runtime validation uses an installation under a separate build/media root
  from the regular desktop. Propagate SYSTEM7_BUILD_DIR through all builders, guest
  launchers and acceptance readers; never hardcode the normal installation in
  a check. Native guest builds use the optimized release profile, including for
  the regular launcher; a host debug xtask must not imply a debug guest.
- P15's native HFS-B boot/default-image cutover is implemented. Reassess earlier
  verification claims under the recovery order above. Preserve its shared
  read/write core, resident/ROM boot, bounded resource-loading and public-SDK
  acceptance gates; host-only verification is insufficient for storage changes.
  Existing no_std apps remain the initial launch gate. Follow docs/HFS-B.md and
  docs/CORE-TOOLBOX-ROADMAP.md for explicit budgets and remaining filesystem APIs.
- HFS-B versions are whole integers (`1`, `2`, `3`), each implying one complete
  disk and Unicode contract. Changing Unicode requires a new filesystem version
  and rebuilding all affected file/name/index representations with collision
  checks; the Unicode header field is validation, not an independent selector.
- Reassess P15 and public Rust Toolbox API audit verification claims
  (see `docs/TOOLBOX-RUST-API.md`) under the recovery order above. The later
  feature sequence is the class-neutral driver/module interface, exercised by
  StuffIt volumes and the vhost filesystem/block-device pair, then the virtual
  host clipboard extension. Bring in the necessary P6 loader/service machinery
  for these milestones; input/network/display extraction and std migration follow
  this initial storage/extension validation sequence.
- Install native driver packages in the blessed `System Folder/Drivers` directory
  and System extensions in `System Folder/Extensions`. Shared module machinery
  does not merge their discovery paths or package/entry contracts.
- Prefer external packages for all future drivers. Keep hardware-specific
  behavior in those packages and resident code focused on generic services,
  admission/lifetimes and the bootstrap support needed to load them. Applications
  call System services; System selects a compatible driver by its declared
  interface and capabilities, never by a driver filename.
- After the removal and audit, define Toolbox acceptance against the actual
  guest services and loaded resources. Use original System behavior and code
  to resolve ambiguity; retain provenance.
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
- Caret blinking is resident TextEdit behavior for every text editing control.
  The shared service owns timing, geometry, clipping and caret XOR; interactive
  idle handling uses that service. A caret-only change never asks an application
  to redraw its text view or window. Rebuild drawing only when committed content
  or geometry is genuinely stale.

Performance bar: the code must not do unnecessary work. Full recomputation is the last resort, not the default: prefer bounded invalidation over full repaints, early-out on no-change, cache what doesn't move, and never do per-event work when per-change work will do. "Input happened" is not "pixels changed." When in doubt, the Cursor Test's responsiveness and the present() trace timings are the benchmarks — the mouse is the profiler.

- Every workflow validation exercise includes relevant unhappy paths and checks informative
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
  never bypass its private lock boundary. The physical buddy pool selects the native invocation gate on metadata access
  and lease disposal; keep its single-hart IRQ/preemption discipline intact.
  Paging does not permit rebasing existing Rust references: initialize stable
  mapped storage before pool/heap owners. Host-thread concurrency tests alone
  do not qualify the native interrupt contract or multi-hart support.
- Use the supplied rustfmt.toml. Run checks appropriate to the change and report
  actual coverage and remaining gaps. A passing subset is not full Toolbox or
  original 68k application compatibility.
- Do not retain dead code or superseded implementations. Remove unused
  placeholders instead of suppressing dead-code warnings; introduce executable
  pieces with their consumers as the staged model becomes resident.
- Public documentation must explain its contracts and remaining gates without
  referring readers to private workspace handoff specs or conversations. Keep
  that communication outside the OS repository; archive completed work in the
  workspace's private `completed/` directory.

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
