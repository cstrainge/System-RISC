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
catalog B-tree, extents overflow B-tree, attributes), with HFS-B's own name
encoding and normalization: **stored names are NFC-normalized UTF-8.**

- Catalog B-tree keys use UTF-8 name bytes instead of UTF-16 units.
- **Decided — maximum name length:** 255 UTF-8 bytes per file or directory
  name component, inclusive. This counts encoded bytes, not Unicode code points
  or grapheme clusters, and excludes the catalog's length/header fields. Reject
  over-limit names without truncating or splitting a UTF-8 sequence. The limit
  applies after NFC normalization, not to the input's original byte count or
  expanded case-folded comparison sequences.
- **Decided — case behavior:** case-insensitive with Unicode-aware, locale-
  independent default case folding, and case-preserving storage/listings.
  Use the default non-Turkic mappings from Unicode's
  [case-folding data](https://www.unicode.org/Public/UCD/latest/ucd/CaseFolding.txt);
  no Turkish-i tailoring and no dependency on the current UI/process locale.
  Pin the folding-data version in the format contract so catalog ordering does
  not change silently on a Unicode/library update. Creation/collision checks and
  lookup use the same comparison; preserve stored spelling rather than replacing
  names with their folded forms. Stored spelling is normalized as specified below.
- **Decided — normalization:** use Unicode NFC for stored names and normalize
  all incoming name components before lookup, create, rename or collision checks.
  Follow [UAX #15](https://www.unicode.org/reports/tr15/) and pin normalization
  data with the format's folding version. Preserve case in the NFC stored name;
  never store a folded search key in place of the display name. NFC performs
  canonical normalization, not NFKC compatibility normalization.
  For comparison, use Unicode
  [canonical caseless matching](https://unicode.org/versions/Unicode18.0.0/core-spec/chapter-3/):
  compare `NFD(DefaultCaseFold(NFD(name)))`, with the decided non-Turkic default
  folding. This comparison form is internal and need not fit the stored-name
  byte limit. Do not substitute lowercase or a locale-dependent collation.
  Validate the resulting NFC stored name against 255 bytes before committing
  changes; failure leaves the catalog unchanged. Bound input/normalization work
  separately without treating 255 input bytes as the stored-name limit.
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

Two decisions remain: timestamp precision/representation and boot policy.
Maximum name length, case behavior and normalization above are settled.

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
   Measure these boundaries after NFC: longer decomposed input that composes
   within 255 bytes is accepted; a name exceeding 255 bytes after normalization
   is rejected even if the original input fit.
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
   Normalization: create decomposed `cafe\u{0301}`, list NFC `café`, and resolve
   composed/decomposed and uppercase equivalents to the same file ID. Reject
   equivalent duplicate creates or renames onto another entry without losing
   either file. A case-only or canonically equivalent rename of the same file
   retains its ID and stores the requested case in NFC. Repeat through remount,
   native and legacy adapters, and path components.
   Include canonical combining-mark ordering and case-fold/normalization edge
   cases. Circled `①` and plain `1` stay distinct; NFC must not become NFKC.
6. Unmount/remount: catalog intact, no repair needed on clean unmount.
7. Boot regression: system still boots from HFS with HFS-B present but
   unmounted-at-boot (option 1).

## Out of scope for this spec

- Live conversion HFS <-> HFS-B (copy tools can come later).
- Journaling (a later gate if crash-recovery demands it).
- The host-directory mount driver (separate track; it can target HFS-B
  semantics for its fork-aware format).
