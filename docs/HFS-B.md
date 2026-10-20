# HFS-B: native UTF-8 filesystem

Status: spec for agent implementation. HFS-B is a new volume format that lives
*alongside* classic HFS. HFS stays frozen as the treaty surface for legacy
apps; HFS-B is the native filesystem for the toolchain workspace, large files,
and everything the 31-char MacRoman world cannot express. This is the same
play Apple ran with HFS+: fork the format, keep both mountable, never stretch
the old one in place.

## Non-goals

- HFS itself does not change. Not the name limits, not the epoch, not the
  B-tree key format. Legacy apps keep exactly the behavior they expect.
- HFS-B is not a compatibility format. No legacy app ever needs to read it
  directly; the File Manager projects a legacy view (below) when asked.

## On-disk format

Base the layout on HFS+'s structure (volume header, allocation bitmap,
catalog B-tree, extents overflow B-tree, attributes) with one deliberate
difference: **names are UTF-8, not UTF-16.**

- Catalog B-tree keys use UTF-8 name bytes instead of UTF-16 units.
- **[DECIDE]** Max name length: 255 bytes (simplest, mirrors HFS+'s 255-unit
  cap) vs. 255 Unicode code points (friendlier, needs counting on insert).
- **[DECIDE]** Case behavior: classic HFS is case-insensitive. HFS-B should
  almost certainly stay case-insensitive (the whole system assumes it), but
  say so explicitly: compare case-insensitively, preserve case as stored.
- **[DECIDE]** Normalization: HFS+ stored names as given (no normalization).
  Recommend the same — store bytes as given, compare per the case rule —
  and document it, because two byte-distinct names can look identical.
- 32-bit allocation blocks minimum; 64-bit file sizes. No 2 GiB ceiling.
- **[DECIDE]** Timestamps: keep the 1904 Mac epoch (continuity with the rest
  of the system) but say the precision explicitly — recommend 64-bit
  microseconds, a clean superset of HFS's one-second fields.
- Resource forks are first-class, same dual-fork model as HFS. The system's
  resource machinery must work identically on HFS-B volumes.

## The File Manager boundary

This is where compatibility lives — per call, like the string shims, not as
a second filesystem.

- **Native calls**: full UTF-8 names, full lengths, full metadata. New Rust
  Toolbox clients see HFS-B as it is.
- **Legacy calls** (Str31 name buffers, classic PBGetCatInfo et al.): names
  come back converted to MacRoman and **truncated to 31 characters**, exactly
  as HFS+ did. Files remain reachable by index and directory/file ID even
  when the name doesn't fit. Unrepresentable characters fail the call loudly
  (BAD_ARGUMENT), never silently — same rule as the string shims.
- The truncation is a *projection*, not storage. The catalog always holds
  the full UTF-8 name.

## Boot story

**[DECIDE]** Two options:

1. **Workspaces only (recommended for now):** the boot ROM keeps reading HFS
   only; HFS-B volumes mount after boot. The blessed System Folder stays on
   HFS; toolchain workspaces live on HFS-B. Smallest change, no boot risk.
2. **Bootable HFS-B:** the boot ROM learns the HFS-B volume header and
   catalog well enough to find the blessed System Folder. Bigger change,
   touches the most trusted code in the system.

Option 1 first; option 2 is a later gate if ever.

## Volume identity

- New volume signature distinct from HFS (`BD`) and HFS+ (`H+`). Suggest
  `HB` ("HFS-B") in the volume header.
- The Finder shows HFS-B volumes normally; Get Info reports the format.
- The blessed-System-Folder ritual works on either format (option 1: HFS
  only for boot, HFS-B for everything else).

## Acceptance

1. Format an HFS-B volume; mount alongside an HFS volume.
2. Create files with 200-byte UTF-8 names (incl. emoji and CJK); native
   calls round-trip the full names; legacy calls return 31-char MacRoman
   truncations; files open by ID regardless.
3. Resource forks round-trip: write a dual-fork file on HFS-B, read both
   forks back, verify against the HFS behavior.
4. Fill past 2 GiB and 4 GiB boundaries; verify sizes and extents.
5. Case-insensitivity: create `Makefile`, verify `makefile` resolves to it,
   original case preserved in listings.
6. Unmount/remount: catalog intact, no repair needed on clean unmount.
7. Boot regression: system still boots from HFS with HFS-B present but
   unmounted-at-boot (option 1).

## Out of scope for this spec

- Live conversion HFS <-> HFS-B (copy tools can come later).
- Journaling (a later gate if crash-recovery demands it).
- The host-directory mount driver (separate track; it can target HFS-B
  semantics for its fork-aware format).
