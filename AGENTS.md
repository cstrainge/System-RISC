# System RISC development

This repository contains only the OS project. Keep the Rust workspace under this
root and the public Toolbox facade in `crates/mac-toolbox`. Finder depends only
on that facade at runtime. Resident implementations belong in System's internal
Toolbox modules; WDEF/CDEF and other definition resources own their unique code.

- Commit completed, verified milestones with descriptive messages. Do not push
  or publish without an explicit request. Keep generated targets, ROMs, disks,
  private working captures, and unrelated Strange Forth code outside this repo.
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
- All system preferences use versioned JSON with typed access and safe concurrent
  transactions. Persistence requires writable storage; do not fake success.
- Every resident Toolbox manager must issue and validate object IDs through
  `system/src/toolbox/handles.rs`; no manager may introduce its own ID scheme.
  Migrate existing ID storage when touching that manager. Preserve generation
  history across clears and retire exhausted slots; register disjoint service
  namespaces centrally and keep ownership checks separate. Keep validation in
  resident storage for both native Rust and legacy trap callers.
- Generate application vers resources from Cargo's resolved package version.
- Repainting is bounded by the changed object: cursor, control, or window area.
  Accumulate exact damage, preserve occlusion and clipping, and leave unrelated
  pixels and drawing work untouched. Movement damages both old and new bounds;
  a local update must never implicitly invalidate the desktop or parent window.
- Use the supplied rustfmt.toml. Run checks appropriate to the change and report
  actual coverage and remaining gaps. A passing subset is not full Toolbox or
  original 68k application compatibility.
