# Verified milestones

## Responsive typing and native navigation

Finder's text-edit path updates Edit-menu policy without synchronous browser or
Trash queries. Key releases stay on this path. Resident menu getters and
idempotent enabled/checked writes leave the compositor clean, and keyboard
arrival no longer forces a stale-screen repaint before event delivery.

The thin TextEdit SDK adds native anchored cursor movement, with Shift+arrows,
Home/End and their Shift/Ctrl combinations in Finder. Original TEKey semantics
remain separately tested. Shared host/RV64 coverage adds49 navigation cases;
private Finder input tests exercise23 physical chord/selection sequences and a
20-character burst. The live profiling fixture verifies zero filesystem calls
while handling text keyDown/keyUp pairs. See [input performance](INPUT-PERFORMANCE.md)
for measured costs and reproduction details. Validation includes the full
`cargo xtask check` gate, host/RV64 Clippy with warnings denied, and the same
canonical typing/selection assertions on headless Windows QEMU.

## Monochrome TextEdit — ABI5 service extension

TEXTEDIT and its paired Pascal conversion trap add a resident editor behind the
thin `toolbox-textedit` SDK and aggregate `System::textedit()` API. Applications
own generation-checked IDs and explicitly dispose editors; resident code owns
text, wrapping, selection, hit testing, editing and clipped drawing.

- Original System7.6.1 probes record86 observations and seven one-bit images.
  Shared public SDK conformance covers356 original layout/edit/hit observations,
  20 native UTF8/clipboard cases and6 Pascal conversion cases on host and RV64.
- Original insertion versus typing, CR wrapping, trailing whitespace, arrow
  behavior and point/hit conventions are preserved for left-aligned Chicago12.
  Native offsets are checked UTF8 byte boundaries, with32767-byte and64-editor
  limits. Failed edits and oversized Paste preserve editor and clipboard state.
- Native Copy/Cut/Paste uses the guarded System clipboard and stages fallible
  edits before publication. Original TextEdit's private scrap is separately
  observed and remains a classic-adapter gap.
- Finder's resource-based Develop > TextEdit Exerciser uses WIND2003, resource
  strings, loaded WDEF0 and resident drawing. Its resizable window supports
  typing, mouse selection, caret blink and contextual Edit-menu commands.
- Seven resident RV64 frames match all86,016 original pixels exactly, including
  clipped text, multiline selection, scroll and controlled caret transitions.
  A second private VM exercises actual Finder menu dispatch, typing, selection,
  Copy/Cut/repeated Paste and resize through real keyboard/mouse input.

Validation: full `cargo xtask check` and host/RV64 Clippy with warnings denied
pass. One initial persistence run reported `ROLLBACK_FAILED` on boot2; the
isolated three-boot test and a complete gate rerun passed. Its cause remains
unidentified; the failed and successful logs are retained outside the repo.

Styled text, other font/alignment profiles, hooks, double-click/autoscroll,
original TEHandle/A-line/private-scrap adapters and editable Dialog Manager
items remain open. See the [resident profile](../system/src/toolbox/textedit/README.md)
and [aggregate tests](../crates/mac-toolbox/tests/README.md).

## Events, Scrap and Finder Clipboard — ABI5 service extension

New independent EVENTS and SCRAP traps preserve the existing ABI5 payloads.
Bindings stay thin, Finder uses only `mac-toolbox`, and resident services own
queues, input state and clipboard storage.

- The captured20-entry OS queue supports posting, masked peek/get, stopping
  flush and event masks; native application posting remains a separate128-entry
  queue. Physical keys, mouse/button, StillDown, wrapping60-Hz ticks and native
  uptime run through public SDK calls. A private RV64 input guest checks raw
  key/button transitions, resource menu interception and queued mouse-up behavior.
- Shared Scrap preserves append/first-match flavor ordering, zero/odd lengths,
  logical padding, clear generations and missing-flavor failures from original
  captures.18 original contracts,17 native Unicode/buffer contracts and12 paired
  Pascal/MacRoman conversion contracts run through host and RV64 services.
  Additional host tests cover byte/record limits, failure preservation, transport
  borrow cleanup and eight OS threads sharing one real guarded clipboard.
- Finder's Edit > Show Clipboard uses MENU/MCMD, WIND2002, resource strings,
  WDEF0 procedure8 and loaded CDEF1 scrollbars. It displays empty or bounded UTF8
  text and reports unsupported formats/oversized content explicitly. Changes are
  observed while the window is open. This read-only viewer is not TextEdit.
- 149,877 strict RGB comparisons match original empty/text windows and the actual
  Finder command path; empty-to-text-to-empty restores the full framebuffer.
  Creation now permits the original window's bottom474 instead of reserving an
  arbitrary lower strip of the native screen.

Validation is `cargo xtask check` plus host/RV64 Clippy with warnings denied.
Original executable probe sources and small captured fixtures/provenance live
under the aggregate test tree; original media and the visible guest are excluded
from test input injection.

Original A-line adapters for the new services, event waits/timers/autorepeat,
process ownership, disk-backed Scrap, styled TextEdit/editable dialogs and full native
scheduling remain open. Device-profile differences and the native clipboard
limits are documented in [Events](../EVENTS.md) and
[Scrap](../system/src/toolbox/scrap/README.md). The remaining manager sequence is
tracked in the [Toolbox plan](../TOOLBOX-PLAN.md).

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
