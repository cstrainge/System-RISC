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
- **Decided — maximum name length:** 255 UTF-8 bytes per file or directory
  name component, inclusive. This counts encoded bytes, not Unicode code points
  or grapheme clusters, and excludes the catalog's length/header fields. Reject
  over-limit names without truncating or splitting a UTF-8 sequence. The limit
  applies to stored names, not expanded case-folded comparison sequences.
- **Decided — case behavior:** case-insensitive with Unicode-aware, locale-
  independent default case folding, and case-preserving storage/listings.
  Use the default non-Turkic mappings from Unicode's
  [case-folding data](https://www.unicode.org/Public/UCD/latest/ucd/CaseFolding.txt);
  no Turkish-i tailoring and no dependency on the current UI/process locale.
  Pin the folding-data version in the format contract so catalog ordering does
  not change silently on a Unicode/library update. Creation/collision checks and
  lookup use the same comparison; preserve stored spelling rather than replacing
  names with their folded forms. Normalization remains a separate open decision.
- **[DECIDE]** Normalization: current recommendation is to store UTF-8 bytes
  as supplied and document the comparison policy, since byte-distinct names can
  look identical. This would differ from HFS+'s decomposed storage
  ([TN1150](https://developer.apple.com/library/archive/technotes/tn/tn1150.html)).
  No normalization policy is settled by the case-folding decision above.
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

Current recommendation: option 1 first, with option 2 as a later gate. The boot
decision remains open.

## Remaining decisions

Three decisions remain: normalization, timestamp precision/representation, and
boot policy. Maximum name length and case behavior above are settled.

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
   Boundary cases accept 255-byte ASCII and valid multibyte names, and reject
   256-byte names on create and rename without changing the catalog. For example,
   63 four-byte emoji plus three ASCII bytes fits; 64 four-byte emoji exceeds
   the limit. Preserve valid UTF-8 and full names through lookup and remount;
   case-fold expansion must not reject a valid stored name within 255 bytes.
3. Resource forks round-trip: write a dual-fork file on HFS-B, read both
   forks back, verify against the HFS behavior.
4. Fill past 2 GiB and 4 GiB boundaries; verify sizes and extents.
5. Case-insensitivity: create `Makefile`, verify `makefile` resolves to it,
   original case preserved in listings. Create `CAFÉ`, verify `café` resolves
   to the same file ID, and reject creating `café` as a second entry in that
   directory. Repeat with the spellings reversed; preserve the original stored
   spelling. Repeat across remount and different process/UI locales. Default
   non-Turkic behavior must also hold: `I`/`i` compare equal, while `I`/`ı` do
   not; locale changes must never alter collisions or B-tree lookup ordering.
6. Unmount/remount: catalog intact, no repair needed on clean unmount.
7. Boot regression: system still boots from HFS with HFS-B present but
   unmounted-at-boot (option 1).

## Out of scope for this spec

- Live conversion HFS <-> HFS-B (copy tools can come later).
- Journaling (a later gate if crash-recovery demands it).
- The host-directory mount driver (separate track; it can target HFS-B
  semantics for its fork-aware format).
