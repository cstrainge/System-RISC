# Verified milestones

## Resource menus and durable Trash — ABI5

Finder uses only the main Toolbox facade. MENU/MCMD resources own labels,
shortcuts, command IDs and initial state; independently loaded MDEF0 owns menu
geometry, pixels and hit testing. Original ALRT/DITL6012, STR#11750 and system
icon families supply Empty Trash. Native error/About/header strings also live
in the application resource fork. Resource Manager string APIs return Rust UTF8;
paired legacy traps convert Pascal/MacRoman at the boundary.

The accepted profile includes:

- Six classic Finder menus with context-sensitive supported commands and disabled
  unfinished commands; mouse tracking and Ctrl equivalents share MenuCommand.
- 111,184 exact original menu pixels plus2,448 public-renderer regression pixels
  for an uncached longest keyless label.
- 43,680 exact original alert pixels, including text, frame, buttons and caution
  icon. Only the amount digit differs between the original8K file and native0K
  empty-folder test and is excluded from that comparison. Modal default/cancel,
  press/leave/reentry/release, blocked background input and desktop-focus
  restoration run through actual userspace services.
- New Folder, drag/Ctrl-Backspace to Trash, Put Away, aggregated per-volume Trash
  browsing and confirmation-gated recursive Empty Trash. Finder and independent
  File Manager fixtures each cold-boot a private writable500MiB disk three times
  to verify persisted state. Both fork contents and CNIDs survive moves/restores;
  deletion reclaims physical allocation blocks.
- Twelve host mutation regressions over real HFS files, independent hfsutils
  checks, reported write/flush rollback and failed-rollback poisoning. Metadata
  trees rebuild within existing reservations without increasing the System heap.
- Serialized native wall-clock reads and checked classic timestamp conversion.
  BootInfo now carries read/write/flush firmware callbacks and is104bytes.

Validation is `cargo xtask check` plus host/RV64 Clippy with warnings denied.
Original media and the user's visible VM are excluded from test input injection.
The desktop runs from a persistent private writable copy; a changed OS build
archives the previous working disk before installing its new image.

This is bounded native functionality. General fork writes, catalog growth,
crash-atomic journaling, collision renaming/Option locked-item policy, full
Menu/Dialog/classic trap coverage, scheduling, native std and JSON preference
transactions remain future work. See [menus/Trash](MENUS-TRASH.md) and
[write limits](HFS-WRITES.md). Existing TrueType/style differences remain recorded.

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
