# HFS-B: native UTF-8 filesystem

Status: **native boot and default System-volume cutover verified, 2026-10-20**.
The shared safe read/write crate, Linux tooling, resident services, ROM and image
builder use HFS-B v1. The full `cargo xtask check` gate has passed: 17 ROM cases,
all shipped application launches, public-SDK persistence/rollback and large-fork
tests, mixed classic HFS mounts and original-system visual comparisons. Existing
no_std applications supply this gate; std migration is a later milestone.

This spec remains the acceptance contract. General streaming userspace fork
writes, resource-fork editing, eject/unmount, exclusive raw-device services and
the graphical Filesystem application remain follow-on work. The shared core and
Linux tool already provide streaming read/write access to both forks. The
current whole-data-file SDK writer stages at most4MiB, and resident resource
loading has a4MiB selected-payload budget on the current8MiB heap, independent of
total fork length. See [the implemented storage architecture](../ARCHITECTURE.md#native-hfs-b-storage).

User-state preservation across regenerated images includes both
`System Folder/Preferences` and `Documents`. The Linux tool provides recursive,
path-based copy-in/copy-out, independent of catalog IDs. Exported bundles preserve
both forks, Finder metadata, comments, Unicode names and precise dates; restoring
them merges directories, with explicit opt-in to replace existing files. Keep
these user exports separately from generated disk cleanup. Preferences remain
versioned JSON; this is file preservation, not a compatibility migration.
See [host-tool commands](../tools/hfs-b/README.md).

HFS-B is a new volume format that lives
*alongside* classic HFS. HFS stays frozen as the treaty surface for legacy
apps; HFS-B is the native startup/System/workspace filesystem after cutover,
supporting large files and names the 31-char MacRoman world cannot express. This is the same
play Apple ran with HFS+: fork the format, keep both mountable, never stretch
the old one in place.

## Non-goals

- HFS itself does not change. Not the name limits, not the epoch, not the
  B-tree key format. Legacy apps keep exactly the behavior they expect.
- HFS-B is not a compatibility format. No legacy app ever needs to read it
  directly; the File Manager projects a legacy view (below) when asked.

## Shared format crate and tooling

Use `crates/mac-hfs-b` as the common Rust filesystem implementation for Linux
tools, boot ROM, resident System and the System RISC Filesystem application.
It is `no_std` with bounded/fallible alloc where required and forbids unsafe
code. It contains the on-disk codecs, name normalization/comparison, timestamp
conversion, layout planning, formatter, structural validator, tree/extent logic
and read/write operations. No host paths, Linux syscalls, QEMU commands, UI,
global device state or private kernel dependencies belong in the core crate.

Supply storage through shared architecture-neutral block read/write/flush traits
with explicit geometry, bounds and read-only/error behavior. Extract the common
interface from classic mac-hfs into a small shared block crate as needed; do not
make HFS-B depend on the classic HFS parser or introduce competing I/O contracts.
Callers supply clock/identity inputs and allocation/scratch budgets. Keep read,
write, format and check profiles separable so the ROM need not carry a complete
formatter or repair utility. Profiles share the same codecs and invariants.

Deliver a Linux Rust command-line tool under `tools/hfs-b` early in P15. Its
planned create/format/check operations must create a sized image, format a chosen
image or unmounted volume range, initialize headers/bitmap/trees/dual-fork/root
metadata, and validate it using the shared crate. Add list/import/export/bless
operations needed to build complete startup images while preserving resources,
Finder metadata, normalized names and precise dates. Raw image/file and device
access are thin Linux std adapters; qcow2 transport/conversion stays outside the
filesystem core. Keep per-image state local rather than relying on hfsutils or
host-global mount state; existing HFS tools cannot encode the new format.

mac-build/xtask must reuse the same crate/tooling to produce HFS-B startup/test
images. Record format/tool versions and build provenance; use the chosen fixed
test clock/identity inputs for reproducible format vectors. Do not invent a
second Python implementation of the layout or formatter for image building.

The resource-based System RISC Filesystem app is a std userspace application
using the public Toolbox for device access, exclusive/unmounted-volume ownership,
progress/cancellation and remount coordination. It can link the same format crate
for image inspection, planning and formatting through a checked SDK block adapter;
resident operations can invoke that crate through the same service contracts.
It must not call private kernel drivers, write an actively mounted filesystem
behind its manager, or reimplement serializers/name rules in the UI. Provide a
native disk/volume service for the required access and generation-checked leases.
The GUI app follows P8 readiness; the shared crate, Linux tools and kernel/ROM
support are immediate P15 work and do not wait for the GUI or std port.

Acceptance must cross these boundaries: format on Linux, inspect/mount/read/write
through System, remount and check with the Linux tool; format through the native
utility/service and check on Linux when that app is delivered. Compare known
serialized vectors and malformed-input decisions across consumers. Exercise flush
failures, read-only/exclusive access, incomplete formatting/cancellation and range
bounds through the real adapters. Canonical resident assertions remain in
crates/mac-toolbox/tests; shared crate tests supplement them without replacing
the public-SDK and real-boot gates.

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
  The filesystem version implies the Unicode version: format 1 uses Unicode
  17.0.0, as specified in [the serialized contract](HFS-B-FORMAT.md). A future
  filesystem version is a whole number (`1`, `2`, `3`), with no major/minor/patch
  or fractional forms such as `1.2` or `2.0`. A Unicode upgrade requires a new
  filesystem version and regeneration of all
  affected file representations, catalog leaf/internal keys and indexes, with
  new-collision checks. The header's Unicode field only validates the implied
  contract; changing it alone cannot upgrade a volume.
  Validate the resulting NFC stored name against 255 bytes before committing
  changes; failure leaves the catalog unchanged. Bound input/normalization work
  separately without treating 255 input bytes as the stored-name limit.
- 32-bit allocation blocks minimum; 64-bit file sizes. No 2 GiB ceiling.
- **Decided — timestamps:** unsigned 64-bit microseconds since
  **1904-01-01 00:00:00 UTC**, retaining the classic Mac epoch. Apply this to
  volume and catalog timestamps; time zones affect display, not stored values.
  Preserve microseconds through native reads/writes and remounts. Zero denotes
  the epoch, not an implicitly missing date; optional-date semantics must be
  represented separately. Use checked integer conversion, never floating point,
  wrapping or silent saturation.
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
- Native date interfaces must carry microsecond precision rather than round-trip
  HFS-B values through the current whole-second Timestamp/clock transport.
  Unix/std conversion uses the 2,082,844,800-second epoch offset with checked
  range and subsecond handling. Legacy HFS-style dates remain unsigned 32-bit
  seconds since 1904: explicitly discard the fractional second for that view,
  reject out-of-range values and never alter the precise stored timestamp.
  Unsupported source precision stays documented; a microsecond field does not
  imply the hardware clock measures at microsecond resolution.

## Boot story

**Decided — bootable and required after cutover:** the boot ROM must recognize
HFS-B, locate its blessed System Folder and load System/Finder resources from
that volume. System mounts the same selected volume with its native writable
backend; startup must not depend on a second HFS disk supplying resources.

An early workspace/mixed-volume slice may precede ROM support during bring-up,
but workspace-only support is not the finished target. Schedule boot support
early, before std/toolchain storage assumptions settle around classic HFS.

The cutover gate requires formatter/validator support, actual HFS-B cold boot
through Finder, launch of all shipped apps, correct fork/metadata persistence
after remount, malformed-media rejection and write/flush-failure tests. Include
the growing System resource fork beyond the old 4 MiB loader ceiling with bounded
memory use. Once this gate passes, image builds and normal startup require an
HFS-B primary System volume and install the blessed System Folder there. Make
the requirement explicit in build/startup validation; incompatible startup media
fails clearly rather than silently selecting an HFS build or fallback volume.

Classic HFS remains mountable, unchanged, for legacy/reference media and file
exchange. Transitional HFS boot checks, if retained, are named recovery/bring-up
profiles rather than a parallel default OS target. Build fresh HFS-B images and
import assets through verified fork/metadata-preserving paths; live in-place
conversion is still out of scope. Prune superseded generated images after checks.

## Decision status

All five high-level decisions are settled: maximum name length, case behavior,
normalization, timestamps and boot. The byte-accurate version-one layout is in
[HFS-B-FORMAT.md](HFS-B-FORMAT.md), with implemented codecs and conformance tests.
Native startup and default-image cutover acceptance have passed; later format
changes still require a whole-number version and rebuilding affected media.

## Volume identity

- New volume signature distinct from HFS (`BD`) and HFS+ (`H+`). Suggest
  `HB` ("HFS-B") in the volume header.
- The Finder shows HFS-B volumes normally; Get Info reports the format.
- Finder Get Info also supports mounted volumes, showing filesystem identity and
  its actual whole-number on-disk version through public Toolbox metadata,
  alongside capacity/free space. Classic HFS has no explicit version field;
  do not invent a fractional version or infer the filesystem from a filename.
- HFS-B supports blessed-System-Folder discovery and becomes the required normal
  startup format at cutover. Classic HFS discovery remains relevant to mounted
  legacy media and any explicitly retained transitional boot profile.

## Acceptance

1. Format an HFS-B volume; mount alongside an HFS volume.
2. Create files with 200-byte UTF-8 names (incl. emoji and CJK); native
   calls round-trip the full names; legacy calls return 31-char MacRoman
   truncations for representable names; files open by ID regardless.
   Unrepresentable names return BAD_ARGUMENT, including when the unrepresentable
   character is beyond the 31-character prefix. Check catalog/index reads,
   incremental directory scans and Standard File results without changing stored
   names; native reads after remount still return the full name.
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
7. Boot: cold boot an HFS-B-only disk through its blessed System Folder to
   Finder and launch all shipped apps. With both formats present, use the selected
   HFS-B installation consistently; reject missing/bad blessing, malformed trees,
   missing/incompatible System/Finder and media that do not satisfy the required
   startup format. Verify System resource forks beyond 4 MiB, boot-to-runtime
   mount identity, persistence through reboot, and mixed-volume HFS access.
   Before System takes over the display, ROM shows the original Happy Mac after
   validating a bootable installation, or the original missing-disk artwork when
   none is found. Verify both states in private VM boots, with resource/reference
   provenance for the original pixels and a checked display handoff to System.
   Kernel startup follows the user-supplied `boot-sample.png`: centered white
   panel with black border/shadow, Happy Mac, "Welcome to System RISC" and a
   striped progress bar over the same background/pattern as the current desktop.
   Reuse the desktop background source rather than copying the sample's background.
   Drive progress from
   real completed initialization stages and repaint only changed progress bounds.
8. Timestamps: exact microsecond round-trips for volume/file/folder metadata,
   create/modify operations and remounts, including 2026 and dates beyond classic
   HFS's 2040 boundary. Verify epoch zero, Unix-epoch conversion, fractional-second
   preservation, legacy second projection and range/overflow errors, including
   rejection of pre-1904 input for this unsigned representation. Precision loss
   must occur only at an explicitly lower-precision compatibility boundary.

## Out of scope for this spec

- Live conversion HFS <-> HFS-B (copy tools can come later).
- Journaling (a later gate if crash-recovery demands it).
- [Vhost host-directory mounts](HOST-FOLDERS.md) and
  [StuffIt archive volumes](ARCHIVE-VOLUMES.md) are separate driver workstreams.
  They share the public filesystem services; the host fork container follows
  native name/metadata semantics without being an HFS-B disk image.
