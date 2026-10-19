# Verified milestones

## Initial OS baseline — ABI4

The first repository milestone includes the existing RV64 boot ROM, blessed HFS
startup, separate resource-fork System and Finder, thin Rust Toolbox clients,
and resource-loaded WDEF/CDEF implementations.

The current accepted extension includes:

- Native UTF-8 and separate Pascal/MacRoman trap paths for all eight existing
  text-bearing service families, plus the original narrow A-line adapter.
- Cargo-derived application versions, checked64-bit native timestamp values and
  actual HFS2026 date/UTF-8 path regressions.
- 86 original control states:269600 exact pixels and8424 original hit records.
  Held drag/grow outlines, cancellation/reentry, zoom round trips and selected
  activation/update behavior use original captures and real userspace guests.
- 23 independently encoded PNG cases, transparency/scaling/clipping/error checks,
  resident decode and a Finder PNG resource exercise. The real Finder framebuffer
  comparison checks69840 natural/scaled/clipped pixels without differences.
- Guarded shared services, one-shot immutable publication, explicit execution
  context shapes and per-thread trap accounting. Eight host OS threads exercise
  one real shared Memory Manager through the public SDK.

Validation uses `cargo xtask check` and host/RV64 Clippy with warnings denied.
The integration harness and small independent/original test fixtures are tracked;
generated reports, full private reference corpus and VM media remain outside.

These are bounded accepted contracts. The real OS scheduler, per-app context
routing, full classic application execution, std port, JSON preference persistence
and x86 boot remain future milestones. Adam7/APNG PNG profiles are unsupported.
Existing TrueType/synthetic-style comparisons retain documented pixel differences;
passing this milestone does not establish complete pixel-perfect Toolbox coverage.
