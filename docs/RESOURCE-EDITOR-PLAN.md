# Resource formats and editor follow-on plan

2026-10-22. Resource authoring and editor architecture against the current
implementation. Implementation is deferred to the planned Res Edit rewrite.
This document records dependencies and contradictions, not an implemented SPI.

Related architecture: [Res Edit](RESEDIT.md) and
[extension composition](EXTENSION-TRAITS-DESIGN.md).

## Agreed direction

- One shared, no_std-capable format implementation supplies binary codecs and
  canonical JSON to mac-build, host derez, the guest editor and clipboard.
- EDLS data drives discovery; RSSC uses existing compact native segment/NABI/RLOC
  machinery. All viewers, including hex and CODE, must migrate through the same
  SPI. No built-in escape hatch in the browser.
- Keep the trait-oriented Rust facade on both sides of a versioned, size-checked
  C-compatible boundary. The trait-model note does not justify sending compiler
  vtables, Vec/String ownership or allocator responsibilities across that ABI.
- A shared module shape does not grant editor code kernel privileges. Editor
  sessions belong to Res Edit; DRVR/INIT retain their separate execution roles.
- Host owns document transactions, undo, file writes and atomic publication;
  editors own private drafts and bounded pane drawing. Subject-file and
  supporting-package resource grants are separate and explicit.
- Generic TMPL and hex supply coverage before bespoke editors. Rich JSON must
  never erase bytes it cannot represent; exact re-encode comparison determines
  eligibility, otherwise an explicit hex/file form preserves the payload.

## Conflicts and gaps to settle in the plan

1. Resource scope is a prerequisite, not an existing driver feature. Current
   ResourceFile is an owned read mark and creates no chain scope. Implement
   thread-scoped nested host guards and pinned package generations first.
2. Current module lifetimes are boot-pinned. Res Edit needs session-owned pins
   and callback draining, not arbitrary unloading of trusted kernel drivers.
3. Unreleased builds still break/rebuild together (AGENTS). Published contracts
   will require independent revision stability; no publication point exists yet, so do
   not add old-project revision wrappers as part of this work.
4. An in-process editor panic terminates Res Edit. Unsaved committed document
   bytes do not survive process termination solely because the host owned them.
   Staged on-disk saves survive; RAM needs a separate journal/service to survive.
   The initial editor design excludes an autosave journal.
5. EDLS's optional alias tail lacks a complete framing/ID-selector encoding.
   Freeze a bounded shared codec, mandatory/default fields and alias-cycle
   rejection before emitting/reading registration bytes.
6. TMPL precedence says later-found wins but also says the edited file wins.
   Make ranks explicit; do not let scan order override the subject-file scope.
7. Code policy has an explicit hex-patching exception, but native generated
   envelope resources are also described as never hand-editable. Resolve that
   distinction before exposing produce through Open With/hex routes.
8. A format_crate declaration is not a Linux codec installation mechanism.
   Host tools link reviewed crates through a generated registry; never execute
   RV64 editor code or fetch arbitrary Cargo packages during manifest decoding.
   Missing rich codecs/templates need located diagnostics and explicit hex output.
9. Current resource-fork readers/snapshots cannot be bypassed with raw file
   replacement. Resource Manager needs owned edit transactions, revision checks,
   stable resource identity, preservation of the data fork and live-reader rules.
10. Selective resource fragments and full-file archive envelopes are different
    containers over identical blocks. Selected export must not renumber resources,
    invent companions, change file association, or erase unrelated dirty flags.
11. FASS is owned by `crates/mac-module/src/file_handlers.rs`, not the
    filesystem interface crate. Its `FAS1` version 1
    codec has an 8-byte header and 12-byte rules: file type, mount action,
    signed icon ID and four reserved zero bytes; no creator field. FSYS v4
    is a separate interface contract. Reconcile the schema with that codec in
    the implementation commit; do not conflate the two version numbers.
    Preserve unknown bits/padding via explicit binary forms when necessary.
12. UI plugins must use managed controls/scroll boxes, pane-local invalidation,
    lazy previews, bounded byte access and no repeated whole-file scans. Keep code
    metadata outside its scroll box and use Monaco for byte/instruction viewers.

## Proposed delivery slices

1. Shared format inventory, canonical diagnostics/JSON writer and source splicing;
   standalone rich/hex round trips, formatting idempotence, strict include conflicts.
2. Resource snapshot/scoping and transactional document substrate, preserving both
   forks, generation/owner checks, failure recovery and unchanged-reader semantics.
3. EDLS/RSSC/editor kit; prove relocation/session ownership using code + hex first;
   replace all statically linked viewers without a privileged host implementation.
4. TMPL interpreter and new-type dogfood (FASS, EDLS, TMPL, copy-window metadata),
   then shared DLOG/DITL/WIND/MENU codecs and visual editor slices.
5. Resource-set clipboard/paste remapping, selective Export/Export Changes/Open
   JSON, original-system fidelity fixtures and the complete per-type acceptance map.

Acceptance stays in public Toolbox Rust tests, with host codec tests alongside
the shared formats and Python retaining only transport/screenshots. Explicit
negative cases: wrong ABI, missing code/codec, alias cycle, duplicate identities,
foreign/stale scopes, malformed TMPL, oversize preview, invalid editor output,
readonly saves, disk/flush/rollback failures and editor panic without file damage.


## Current implementation boundaries

- `apps/resedit/src/viewers/mod.rs` statically selects viewers. EDLS discovery,
  RSSC sessions and editable resource documents do not exist yet.
- `toolbox/resources/src/file.rs` supplies owned reads, not editor resource-chain
  scope or transactional Resource Manager writes. File Manager fork writers are
  useful storage primitives, not a substitute for resource identity/transactions.
- `crates/mac-code-resource/src/code_types.rs` contains the standard code-family
  registry; RSSC must be registered with its invocation contract, validation and
  loader tests in the same slice as editor packaging.
- `tools/mac-build/src/manifest.rs` remains the current authoring compiler. The
  planned complete Macintosh resource catalog is a target, not current coverage.
- Driver discovery stays in System Folder/Drivers; extensions stay in Extensions.
  Editor discovery must not borrow trusted kernel execution merely because its
  packaging reuses native code resources.

## Per-slice acceptance

Codec work: byte-identical binary/JSON/binary round trips, raw fallback for
unrepresented bytes, canonical formatting idempotence, duplicate/include
conflicts and located errors. Generate an explicit catalog coverage table;
unsupported forms must be reported rather than silently treated as implemented.

Resource substrate: stale/foreign handles, scope nesting/unwind, file revision
conflicts, readers during edits, both-fork preservation, readonly refusal,
failed save/flush/cancel and restoration of the original on-disk file.

Editor host: relocated code at multiple bases, all viewer dispatch through the
SPI, EDLS aliases/modes/precedence, malformed registry/output, missing codecs,
owned controls and callback cleanup. In-process panic may terminate Res Edit;
it must not corrupt the previously saved file. Surviving unsaved document RAM
requires a separate journal/service decision.

UI and source bridge: original fixtures for DLOG/DITL/WIND/MENU, selective
exports preserving IDs and unrelated dirty state, clipboard remapping as an
atomic set, and editing two Get Info dialog items followed by export/recompile
whose resource diff contains only those edits. Trace pane
scrolls and selection changes: unchanged headers, zero whole-fork rescans and
bounded changed-pane damage. Retain full code metadata outside the scroll view.

No new compatibility path for old unreleased System RISC artifacts is needed.
Rebuild all producers and consumers together; preserve explicit compatibility
with original Macintosh resources and other external formats.
