# Native resource executable format, version 1

The separate native disk contract is documented in
[HFS-B on-disk format, version 1](docs/HFS-B-FORMAT.md). Its format version also
fixes its Unicode name contract; changing those tables requires rebuilding every
affected file/name/index representation, not changing an independent setting.

Native packages retain Macintosh file metadata and first-class data/resource
forks. Their resource fork uses the native RRF1 layout below. Original Macintosh
forks retain their classic layout and are decoded separately from that explicit
marker; copying an original application never rewrites its forks. Host transport
uses MacBinary II with a valid CRC-16/XMODEM header and 128-byte fork padding.
MacBinary file names and serialized resource names use Mac Roman.

## Resource-fork containers

mac-build emits RRF1 for current native packages. All fields are big endian.
Its 16-byte header is:

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | Zero discriminator (invalid as a classic data offset) |
| 4 | 4 | Magic/version `RRF1` |
| 8 | 4 | Data-area byte length |
| 12 | 4 | Map byte length |

Data begins at byte 256; bytes 16 through 255 are zero padding. The map begins
immediately after the data area and ends exactly at EOF. The total fork length
must fit u32. Unknown native versions require rebuilding/recreating the file;
they never fall back to the classic decoder.

The map's type list, Pascal/MacRoman names and 12-byte reference records retain
classic geometry. A native reference contains ID i16 at 0, name offset u16 at 2,
attributes u8 at 4, full u32 data-area offset at 5, and three zero reserved bytes
at 9. Each payload still starts with a u32 length. Map/name/reference-list offsets
remain u16; this change expands data addressing, not those separate limits.

Original forks use their standard four-u32 header and 24-bit reference offset
at bytes 5 through 7. Bytes 8 through 11 are classic handle storage and never
contribute to that offset. Both the indexed and streaming readers consume the
same checked format discriminator and reference parser. No format is guessed
from resource types or payload contents. Copying and storage preserve fork bytes.

Address width is independent of allocation policy. Selected reads can inspect
a resource past 16 MiB without reading preceding payloads; payload/map/count
budgets remain checked before allocation. The current default selected-payload
budget remains 4 MiB until the page-backed heap policies are active.

The public ResourceFile format query reports Classic or SystemRisc. Lossless
JSON archives declare `resource_format` as `classic` or `system_risc`; rebuild
requires that field and uses its corresponding encoder. Res Edit exports the
actual format along with raw resource payloads, names and attributes. Normal
build manifests use native RRF1 regardless of architecture.

Native System/application executables use an `'RV64'` resource ID 0 and a companion `'NABI'`
resource ID 0. The unreleased format is **version 1**. ELF is a build-time input,
never a runtime resource container. The old unreleased ELF-backed layout is
replaced, not supported by a second decoder. Rebuild ROM, System, Finder,
applications and definitions together when this contract changes.

The [unreleased compatibility policy](ARCHITECTURE.md#unreleased-compatibility-policy)
applies to all native formats, ABIs and schemas. Update every producer/consumer
and regenerate project artifacts when changing a layout; maintaining previous
System RISC layouts, automatic migrations or fallback decoders is not required.
Keep validation and clear rebuild/recreate diagnostics for incompatible input.

## Native image resource scopes (development ABI 1)

Native resource selection is a 16-byte C-layout record: `kind: u32`, zero
`reserved: u32`, and `image: u64`. Kinds are System (0), search chain (1),
application (2), and explicit image (3). The first three require image zero;
explicit images require a nonzero facility-issued alias ID. These values never
select an execution identity or grant access to mutable application state.
Classic selectors are translated at the classic boundary; native scopes never
encode slot indices or generations in a signed field.

ProcessInfo carries the readonly image alias ID, zero while admission is
incomplete; the Rust SDK exposes an optional ImageScope. Application resource
replies capture the actual image scope so a saved view cannot silently retarget
another image. Ordinary lookup rejects invalidated aliases immediately, while
an authentic retained invocation may finish through its original admission
receipt. A scope value itself does not pin source bytes.

## NABI descriptor

NABI has exactly 40 bytes. All numeric fields are big endian.

The ABI number remains 1 during unreleased development. Its layout fingerprint
is computed from the actual native wire sizes, offsets and scope tags in one
shared crate. A mismatch, including the old zero field, is rejected before entry
with an explicit whole-world rebuild error. It is a compatibility check, not
code authentication or an execution permission.

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | Magic `RVN1` |
| 4 | 2 | Format version: 1 |
| 6 | 2 | Role: 1 = resident System, 2 = application, 3 = code resource |
| 8 | 4 | Native System service ABI version: 1 until the first proper release |
| 12 | 4 | Native ABI layout fingerprint (`mac-abi::NATIVE_LAYOUT_FINGERPRINT`) |
| 16 | 8 | Inclusive preferred load reservation base |
| 24 | 8 | Exclusive preferred load reservation limit |
| 32 | 8 | Linked entry point |

The role, ABI and reservation meanings are unchanged. Entry is explicit because
there is no ELF header at runtime. It must be even and lie inside initialized
executable segment bytes. Versions, layout fingerprints, lengths, address overflows
and roles are checked. The obsolete 32-byte NABI layout is rejected with a rebuild
error. There is no fallback to ELF.

## RV64 segment stream

The header is followed immediately by interleaved segment records and bytes.
All metadata is big endian; machine instructions/data retain their original byte
order. There are no alignment paddings, section tables, ELF/program headers,
symbol tables, dynamic-link metadata, BSS payloads or trailing bytes.

| Header offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | Magic `RV64` |
| 4 | 2 | Format version: 1 |
| 6 | 2 | Segment count, 1 through 256 |

Each record has this layout, relative to its start:

| Record offset | Size | Field |
| --- | --- | --- |
| 0 | 8 | Destination offset from NABI load base |
| 8 | 8 | Length of initialized bytes |
| 16 | 4 | Permissions: R = 4, W = 2, X = 1 |
| 20 | length | Raw initialized bytes |

The next record begins at `20 + length`, without padding. Records are ordered by
increasing destination offset, nonempty, nonoverlapping and entirely inside the
NABI reservation. Unknown flag bits and W+X are rejected. Permissions describe
intent; enforcing them awaits memory protection. BSS-only segments are omitted.
Every byte of the reservation not supplied by a record is implicitly zero,
including inter-segment gaps and trailing BSS through the reservation limit.

The loader validates the entire stream and all relocation patches before writing
RAM. It reserves the full declared span at a disjoint actual base, zeros that
span, copies each record to `actual_base + offset`, applies RLOC, synchronizes
instruction fetch (`fence.i`), and enters
`actual_base + (NABI.entry - NABI.load_base)`. Zeroing precedes copies/patches so
initialized data and relocated pointers survive. System uses its fixed board
reservation; applications and definitions are independently relocatable.

The build links RVA23U64-profile RV64 ELF64 System V `ET_EXEC` binaries using
the LP64 calling convention (the Rust target name remains
`riscv64imac-unknown-none-elf`, with full `+rva23u64`). It validates them
with `elf::decode`, and extracts only initialized PT_LOAD bytes. Dynamic linking,
interpreters, TLS, unsupported machine/ABI flags, bad alignment and overlapping
ELF segments fail packaging. Link relaxation remains disabled. Full debug ELFs
and stripped host artifacts can remain in build output; none is stored in a
runtime code resource. The segment stream is the intended output shape for a
future 68k translator too; implementing that translator is outside this change.

## RLOC and native definitions

Native ABI1 carries `ProgressRange` in `ControlDefinitionState`: u64 completed,
in-progress and total counts; u32 determinate and zero-reserved fields. The CDEF
projects these directly onto its pixel geometry using checked u128 arithmetic.
No intermediate16-bit percentage is stored. `ControlChange` returns up to two
rectangles for separate fill/outline changes. Native `ControlCall`/definition
state sizes change together; rebuild System, every app and all definitions.
The code-resource envelope remains format1, and no earlier native ABI path is
retained. Classic68k control records/procIDs remain at their existing adapter.


System's native controls have descriptive resource names and independent Cargo
artifacts: CDEF0 “Buttons, Checkboxes and Radio Buttons” (`CDEF_BUTTONS`), CDEF1
“Scroll Bar” (`CDEF_SCROLL_BAR`), CDEF-32760 “Progress Bar”
(`CDEF_PROGRESS_BAR`), and CDEF-32759 “Static Text” (`CDEF_STATIC_TEXT`). The
negative IDs and native-only procedure discriminators are declared in
`mac_abi::controls`; they do not change the classic procID dispatch contract.
Progress Bar message6 takes a borrowed `ControlChange` (before/after typed
standard states and two output rectangles) and returns only its own changed fill
bounds. Empty rectangles mean no pixel change. System validates that damage stays
inside the control, and retains the new state even if no repaint is needed.

Finder cpUI8751 is the76-byte, big-endian CpW1 projection of original fvew8751:
magic4, content dimensions u16×2, procedure i16, zero u16, font family/size i16×2,
face u16, zero u16, six top/left/bottom/right i16 rectangles, UTF-8 Stop caption
length u8 and bytes, then zero padding. The full source-resource provenance is
`provenance/finder-copy-window.json`. The six rectangles are remaining label,
remaining count, operation, filename, progress and Stop, in that order. No
native struct bytes or executable code are stored in this layout resource.


RLOC is unchanged: `RLC1`, a little-endian u32 count, then 8-byte records containing
little-endian u32 offset from the preferred base, u8 pointer width (4 or 8), and
three zero bytes. Count is bounded at 65,536. Records are ordered and
nonoverlapping, patch sources must lie in initialized segment bytes, and linked
pointer values must lie inside the declared reservation or equal its exclusive
limit (a one-past-the-end pointer).
Relocation changes absolute internal pointers; internal PC-relative references
move unchanged. Invalid records reject the whole load before any writes.

The packager accepts internal R_RISCV_32/64 pointers and internal PC-relative
calls/addressing/branches (16..19,23..25,44,45,57). Other relocation kinds fail.
Application and code-resource builds emit relocations and the linker marker
`__system_risc_relocation_v1=1`. RLOC is required for both, including an empty
8-byte table when no absolute pointers need patching; there is no fixed-module
fallback. System is fixed-linked and has no RLOC.

A native definition stays in its original type and ID (`WDEF`, `CDEF`, `MDEF`,
`LDEF`, etc.). Its payload is **NABI descriptor + RV64 segment stream + RLOC**.
The segment count/lengths determine exactly where RLOC starts; the RLOC count
fixes the envelope's end. There is no nested resource fork or ELF suffix. The
same decoder and relocation planner serve applications and definitions. Definition
placement is independent of linked addresses; current System allocates its
`0x82000000..0x83000000` module pool from the top down, and records actual ranges
for collision checks. A definition's entry ABI and variation dispatch remain
specific to its resource family. Translation routing remains unchanged.

Before entry, provide a 16-byte-aligned stack and the established privilege/trap
environment. Module entries are C functions, taking their first argument in
`a0`, returning in `a0`, with valid `ra`. They do not initialize hardware, stacks
or their own BSS. All native code remains trusted shared-address-space code.

The exact shared structures are in
[`crates/mac-abi`](crates/mac-abi/src/lib.rs). Entry/service tables and definition
calls start with two
native-endian `u32` fields: ABI version and structure size. Version must match;
size must be at least the known structure size. Only read a full structure
after validating its header. Native pointers/function pointers are 64 bits.
These structures must never be written directly as classic 68k disk structures.

System entry:

```text
unsafe extern "C" fn(*const BootInfo) -> *const SystemServices
```

System is initialized once on one hart, before concurrent execution. The
bootstrap supplies RAM information, an optional logging callback, and an optional
firmware block-device interface and selected startup disk/partition identity.
System mounts that disk through its resident File Manager and reads maps and
selected resource payloads with bounded allocations. There is no whole-fork
firmware callback. BootInfo.flags bit0 requests graphical startup; without it
System returns a non-graphical service table for the isolated ABI smoke test.
Incompatible arguments return null. The System image stays resident.

Application entry:

```text
unsafe extern "C" fn(*const SystemServices) -> i32
```

An application validates the table before calling the loader-supplied trap gate.
ABI 1's `SystemServices` is 24 bytes on RV64:

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 8 | ABI version 1 and structure size |
| 8 | 8 | `TrapGate` function pointer |
| 16 | 8 | Capability bits |

The gate's C signature is
`unsafe extern "C" fn(number: u32, request: *mut (), size: usize) -> i64`.
The resident RV64 gate places the number in `a7`, payload address in `a0`, and
payload size in `a1`, then executes `ecall`. System validates the request layout
and dispatches it to its internal manager modules. The result is returned in
`a0`; individual payload contracts describe output fields. Native trap numbers
and payloads are defined in `mac-abi/src/traps.rs`. Stateful services reject
recursive runtime access; drawing/font services can be called from a loaded
WDEF/CDEF while the Window Manager is dispatching that definition.

Finder opens its startup-volume catalog window and runs its event loop when graphics is available;
it logs and returns zero for the headless ABI harness. There is one foreground
application; independently scheduled application OS threads enter services with
trusted app/thread identities and virtualized legacy globals. All code currently
runs in S-mode with Sv39 active but trusted shared mappings and without
user-process isolation. Payload pointers must obey their
trusted native ABI contracts; size/alignment checks do not establish memory
protection. Public request/event structures use their declared `repr(C)` layout,
without an implicit header.

The CLASSIC payload carries an original A-line trap number, 32-bit registers and
a checked guest-memory slice. The initial adapter supports SetRect, OffsetRect,
InsetRect, SectRect, UnionRect, EmptyRect, EqualRect, PtInRect, GetNextEvent,
EventAvail and TickCount, point/rectangle mapping, basic text/font operations
and monochrome cursors. It preserves the original Pascal stack conventions,
big-endian data and result slots. Unsupported traps return `NOT_SUPPORTED`
without changing guest registers or memory. A 68k execution engine and classic
WindowRecord-dependent services are not implemented.

Drawing destinations use an explicit native `Target` record: `kind: u32`,
`reserved: u32` (zero), `id: u64`. Kinds are Window (0), Desktop (1), and
Offscreen World (2). Desktop requires ID zero; windows and worlds validate
opaque object identities with their respective resident owner checks. No kind
is encoded in token bits. Rust applications use `DrawingTarget` / `WorldId`
through the Toolbox; the binding encodes this record. Borrowed definition
surfaces use the default Window/zero target and cannot combine that surface
with another destination. Retained scene/cache/damage keys distinguish windows
and the desktop explicitly. Window and Offscreen World identities, including
`WindowRequest.id`, drawing/control/dialog/drag window references and
`WorldCall.world`, are opaque native u64 tokens. No window token is truncated
or packed into another identity.

Offscreen Worlds use the registered GWLD object and GWST backing/state lifetimes.
The generic owner walk releases both, retaining quarantined payloads/backing
until retry. Pixel usage is cached per owner and changes only at allocation or
release. The current 512-Kpixel per-owner raster budget remains a bring-up
limit until accounted page heaps replace it; it is not a quota on world count.

Native QuickDraw, font, region, bitmap, offscreen-world, cursor, mapping, picture,
icon and Resource Manager request layouts live in `mac-abi/src/quickdraw.rs`
and `resources.rs`, with their operation numbers in `traps.rs`. Input byte and
point slices are borrowed for the call; window replay retains owned snapshots.
Read-only resource results identify their originating file, preserving lookup
identity when the application shadows a System ID. Resource reads copy into
caller-provided storage. Resource pointers, native wire structures and UTF-8
names are not substitutes for classic 68k Handle, GrafPort or MacRoman records.

The separate QEMU ABI smoke harness materializes the exact packaged segment
resources through the shared load planner into raw memory images. QEMU loads
those raw reservations for the small ABI harness. The [boot ROM](bootrom/README.md)
separately tests real virtio/HFS-B/resource/segment loading; System loads Finder and
definition resources through its resident filesystem service. Normal disk-boot checks wait
for `DESKTOP_READY`. Rebuild the complete unreleased system on format changes.
See [CODE-RESOURCES.md](CODE-RESOURCES.md) for executable resource families.

The current BootInfo is 176 bytes on RV64; SystemServices remains24bytes. BootInfo
contains RAM/log information, firmware disk-info/read/write/flush callbacks,
startup disk/partition identity and flags. Device ordinals remain stable while
System runs; NOT_FOUND ends enumeration. Each read/write transfers exactly512
bytes; flush is an explicit durability barrier. Callback contexts and DMA queues
remain firmware-owned and are serialized by the resident owner guard. DiskInfo
is24bytes and carries sector count, read-only state and driver icon identity.
Missing write/flush callbacks make storage read-only. Unknown native artifacts
must be rebuilt together; no earlier BootInfo layout is decoded.

At byte144, BootInfo carries a physical pointer to enabled CPU records, followed
at byte152 by a native `usize` count. Each record is `{ hart: u64 }`, sorted by
hardware ID with duplicates rejected; sparse IDs are preserved. The array lives
in reserved firmware RAM until reset and includes the actual primary hart. The
ROM checks `/cpus` address-cell width (one or two), zero size cells, one register
ID per CPU, and recognized status strings. Missing status means enabled; `ok`
and `okay` enable a node; `disabled`, `reserved`, `fail` and `fail-<code>` exclude
it. The enabled list is fallibly allocated after allocation-free structural and
primary-membership preflight, then sorted and checked before publication. System
checks the mapped extent, reservation and shared topology contract at admission.
CPU records describe discovery only: they neither authenticate the current hart
nor release any secondary from the ROM park. BootInfo's ABI version remains1;
its Header size must equal176bytes before the enlarged structure is read.
Rebuild ROM and System together; previous layouts are rejected.

At byte160, BootInfo carries a mandatory physical machine-monitor directory
pointer; byte168 holds its mandatory layout fingerprint. The fingerprint covers
all shared sizes, alignments, offsets and protocol constants. The40byte directory
is `{ entries: physical pointer, count: usize, stack_bytes: u64, timebase_hz: u64,
image: physical pointer }`. Each32byte entry is `{ hart: u64, msip: u64,
storage: BootMemoryRange }`, in checked topology order. All arrays, the80byte
BootImage and disjoint page-aligned control backing remain reserved until reset.
The checked DT timebase and CPU interrupt-controller/CLINT route pairs determine
clock and MSIP addresses; numeric hart IDs are not MMIO offsets. Unsupported
bindings or translated buses refuse boot.

BootImage contains ten atomic u64s: sealed, entry, image base, image bytes,
required physical limit, required ISA, vector bytes, hardware tag mask, fault
entry and returned entry. The ROM seals the actual validated NABI load plan once
before the primary's nonreturning handoff. Input cells become immutable before
sealed=1 is release-published. System entry uses a0=payload, a1=exact payload
bytes and a2=nonzero role: BOOT=0x424f4f54 or HART=0x48415254. Unknown/zero roles
are rejected before cold initialization. Secondary entry is this same sealed
System entry, never a caller-supplied callback.

Each464byte Control begins with ten64bit words: hart, MSIP, requested,
acknowledged, stop, stopped, joined, command, signed result and entry guard.
Its96byte SecondaryStart begins at80: sequence, firmware-root SATP, control
virtual address, service top, phase, admission step, observed, expected, vector
bytes, hardware tag mask, PMP address and configuration. Wake request/ACK epochs
follow at176/184. The272byte integer frame starts at192 (32registers, PC, status).
Bindings borrow only individual atomic cells or raw scalars, never a shared
Control/frame reference across assembly mutation.

Joined1 denotes a peer receiver joined from M park; joined2 denotes the
primary receiver. The joined field is not the current privilege/entry phase. FenceI=1
ACKs actual remote fence.i. START=2 is a one-shot transition from the real M park:
M independently validates the selected CPU profile/root/physical permissions and
reports Starting=1 or Refused=3. Real warm System initialization reports
AdmittedIdle=2; the initial phase is Prepared=0. A separate nonzero wake epoch
must receive an actual S interrupt ACK before the core mints its unique readiness
owner. M injection alone grants no readiness or scheduling authority. Stop is
sticky and irreversible; only the M receiver ACKs after abandonment. No old
monitor layout or raw-memory boot fallback is accepted. Rebuild the entire world
when this unreleased ABI1 contract changes.

At byte88, BootInfo appends a pointer/count pair for RAM banks, followed at
byte104 by a pointer/count pair for reservations. Counts are native usize
(RV64: u64); each extent is `{ base: u64, length: u64 }`. Arrays are borrowed
from pinned firmware storage for the System lifetime. The existing RAM summary
is derived from bank0 and must agree with it; it is not a configured world size.
Every extent is nonempty, page aligned and checked for overflow. RAM banks do
not overlap. Reservations are disjoint and wholly within one bank. System
validates the complete account at entry. The ROM discovers all enabled memory
banks and firmware reservations from the DTB; its reservation union includes
build-defined firmware state/heap, the module arena, the FDT tail and the loaded
System reservation (including its static heap). Frontbuffer, tracking scratch
and compositor backbuffer occupy three disjoint page-aligned frames in the
platform display reservation; the same checked record emits their linker symbols,
resident constants and boot reservation. They are initialized before use and do
not consume the System image's BSS. Later loader
allocations must join the same account when the page layer becomes live.

At byte120 is the 16-byte display-scratch extent. Byte136 holds a physical
pointer to a 72-byte execution record: hart, root physical address and hardware
ASID mask (three u64s), physical and virtual System extents (two16-byte ranges),
then a physical pointer/count pair for mapping records. Each 32-byte mapping
record is `{ physical: range, virtual_base: u64, flags: u32, reserved: u32 }`.
Flags are READ=1, WRITE=2, EXECUTE=4, FIRMWARE_ONLY=8; reserved is zero. These are
bootstrap permissions, distinct from the native segment's ELF-style flag bits.
System records preserve physical/virtual offsets, reject writable executable
pages and overlap, and include writable non-executable zero-only tail pages.
Initialized segments and BSS start on separate pages. Opaque firmware extents
are supervisor-only identity mappings in the firmware root, absent from the
resident root. Their backing and every handoff array stay reserved until reset.

Firmware initializes System's reservation at its physical storage address while
the code is linked to the checked high virtual range. It prepares an owned
supervisor-only root and checks actual CPU privilege, delegation, timer and
translation capabilities before MRET. Only scalar address encodings cross that
boundary; the resident creates fresh checked mapped views before pool, heap or
thread owners exist. System subsequently installs real PageBox-owned roots,
not model-only mapping plans. Firmware callbacks run under a masked, hart-local
RAII root scope; all resident addresses retain their mappings. The ABI probe
uses the same handoff, page-record builder and architecture admission source.
No native M-mode fallback or earlier execution-record decoder exists.

`platforms/<name>.json` defines physical placement policy. mac-build rejects
overlap, misalignment, arithmetic overflow and ranges outside minimum RAM before
emitting linker symbols or Rust constants. The ROM link script, flash packager,
module allocator, relocation arena and current System heap consume that checked
record. Actual RAM size remains a boot discovery. The same checked record supplies
the System virtual execution range, physmap window, guarded System-stack window
and `virtual_memory.system_heap_base`. The heap limit is derived from that base
plus `memory.system_heap_bytes`; it must be canonical, page aligned and disjoint
from the complete image, stack window and physmap, inside the System range.

Before preparing its permanent roots, System admits one physical buddy lease for
the fixed heap. Both roots map its configured usable capacity supervisor RW/NX;
the owner remains pinned beside those roots until reset. A buddy-rounded tail is
retained physical backing, not extra usable heap. The ordinary allocator starts
only after the root is installed. The selected QemuRISC-V capacity is 512 MiB;
smaller machines may refuse that admission despite satisfying static placement
checks. They report the requested capacity and remedy, never silently shrink it.
The checked `memory.system_recovery_bytes` reserves 32 MiB inside that same
backing. Only the remaining 480 MiB is ordinary allocator capacity. Terminal
frame/stack storage consumes unique protected subranges; image-backed terminal
state/assets stay separately pinned. Domain heap/metadata charges, the complete
recovery-service kit and per-process protection remain staged work.

Native catalog and volume names have256-byte UTF8 buffers, enough for255-byte
NFC names plus a spare byte. Catalog dates are unsigned64-bit microseconds since
1904, with a separate backup-presence flag so the actual epoch is representable.
VolumeInfo carries a big-endian FourCC value in its filesystem-kind u32 and a
whole filesystem version (HFS has no
explicit disk version; HFS-B currently reports1) and an explicit blessed-folder
CNID; HFS-B does not overload the Finder-info bytes to carry its blessing.
Builtin identities are `HFS ` and `HFSB`; provider identities must be four
printable ASCII bytes (space permitted, all-space rejected), not private enum
ordinals. The old reserved u16 is `capacity_known`: 0 requires zero total/free
fields and means unknown; 1 requires free <= total. A disk ordinal of `u32::MAX`
with partition-start zero identifies a filesystem mount without a block device.
The public Rust SDK exposes `Option<VolumeCapacity>` and `VolumeSource`, with
provider FourCC/version carried by `Filesystem::Driver`; it exposes no sentinel.
CatalogInfo and VolumeInfo are
each368bytes; FileCall is832bytes on RV64. Native clock results carry signed Unix
seconds and a checked microsecond fraction; classic seconds projections are
explicit and reject dates outside their32-bit range.

Window and retained drawing titles accept768 UTF8 bytes, enough for all255
MacRoman characters after conversion. Legacy Pascal/MacRoman conversion remains
at the trap boundary; native application interfaces expose Rust strings.

Graphics operations36/37/38 begin, commit and cancel an atomic masked content
update. BEGIN_MASK accepts an encoded QuickDraw region in window-local
coordinates, or screen coordinates for the Finder-owned `DESKTOP_TARGET`.
It intersects the region with the target bounds and rejects an empty mask.
Window generation/owner checks apply; only Finder can mutate the desktop.
Draws and port state remain
private until commit; dropping the SDK `MaskedUpdate` cancels them. Nested
updates are rejected. Commit replaces retained drawing and TextEdit hover policy
only inside the mask, preserving the rest of the scene and pending exposure
damage. Repeated masks replace prior fragments rather than accumulating drawing
history. Exact damaged spans invalidate the pixel cache; menus and unrelated
window content retain their cached pixels. Desktop patches restore only damaged
background spans and subtract every visible window structure before presenting;
transparent patches expose the desktop pattern rather than stale pixels.
Normal BeginUpdate/EndUpdate still replace a complete scene.

Graphics operation39 begins the same transaction from a bounded rectangle list:
at most1056 records, each8bytes, four little-endian i16 values in left/top/right/
bottom order. System sweeps edges into an exact region before staging. This avoids
one region trap per selected item. Operation40 sets a transient content outline
(verb0 removes it; verb1 supplies bounds). It follows an open drawing transaction's
commit/cancellation. Only the active window, or the selected desktop, shows
tracking feedback. Desktop outlines remain beneath window structures. System
temporarily paints visible outline pixels alongside its cursor overlay, presents
the damaged edges, and restores the underlying backbuffer. Outline motion never
appends retained commands or enters the scene compositor. Menu/window tracking
temporarily suppresses that feedback.

Finder's `gSum`20031 descriptor contains `SUM1`, big-endian i16 width/height,
then four left/top/right/bottom rectangles for count, logical size, allocated
size and error status (40bytes total). `STR#`20030 supplies its labels. This
Multiple Items window owns a stable selection snapshot and computes recursive
totals in bounded steps, replacing only its value rows when finished.

FILE requests and metadata are defined in mac-abi/src/files.rs. File IDs are
persisted catalog identities, scoped by the generation-checked mounted volume.
Operation34 `SET_ICON_LOCATION` uses `fork=0/index=0` to clear placement, or
`fork=1` and `index=(u16(h) | u16(v)<<16)` to save signed coordinates. The point
(-1,-1) is reserved for unplaced items. `position` supplies the checked 1904-us
catalog-change timestamp. The operation changes only Finder info's location at
bytes10–13 and the initialization flag0x0100 at bytes8–9, preserving object dates,
both forks and every unrelated Finder byte. Identical state performs no disk
write or watch notification and does not invalidate an existing directory scan.
HFS and HFS-B implement this through their shared filesystem facade; read-only
mounts reject it. Writable provider support awaits FSYS metadata write-back and
must return an error rather than pretend it saved. Legacy Pascal replies project
the name to Str31; native replies retain full UTF-8 names. Finder reads this same
catalog metadata after close/reopen and rename, rather than keeping a path-keyed
position map.

Operation35 `MOVE_INTO` changes the existing `cnid`'s parent to `index` in the
same mounted volume. `position` is the checked catalog-change timestamp. The
name, CNID, file dates, both forks and complete Finder metadata (including icon
placement) are preserved. Self/descendant moves, name collisions and protected
objects are rejected before publication. Both source and destination directory
watches are notified only after success; the same parent is a no-op. Cross-volume
copy/move has distinct identity and transaction requirements and is not encoded
as this operation. Legacy Pascal replies project the result name to Str31.

References pair opaque mounted-volume IDs with persistent catalog IDs; volume
and open handles include a generation. Reads copy into caller storage, and both forks have independent
marks. Native slash paths are distinct from classic colon paths. CONTROL requests
use mac-abi/src/controls.rs and dispatch CDEF0 (standard controls) or CDEF1
(scrollbars); their inline state does not expose classic ControlHandle or
WindowRecord ABI. CONTROL and PSTRING_CONTROL share resident managed objects:
operations10/11 create standard/scrollbar objects;12 configures,13 queries,
14 disposes,15 draws,16 begins tracking,17 polls and18 cancels. ControlCall now
also carries id/flags/ticks/step/page. Rebuild all modules for this unreleased
layout; there is no old-size decoder. IDs use the central generation facility
with a distinct Controls namespace; close/app exit retires parent-owned objects.
The Pascal trap converts standard-control title input/output while retaining the
same identity/owner checks. Rust apps use only the UTF-8 SDK surface.

Tracking flags report held/tracking, handled, action and visual/value change.
Buttons/checks/radios report an action only on release inside the original part;
leave/re-enter changes only their highlight, and cancel/disable/resize/deactivation
ends tracking without activation. Scrollbars use CDEF1 hit/thumb math with bounded
arrow/page repetition. Managed drawing replaces the retained command for its ID
and damages only old/new control bounds. Unchanged polls do not append commands
or invalidate content. See [the managed control SDK](toolbox/controls/README.md).

WINDOW operation8 (`SET_GROW_ICON_CLIP`) adds no structure fields: request.bounds
is a window-local clip for automatic WDEF grow drawing when reserved=0.
reserved=1 restores unrestricted drawing; other values are rejected. An empty
clip suppresses drawing. This leaves application-owned scrollbar borders and
headers under application control while WDEF retains its drawing implementation.

MEMORY0x10014 uses `mac-abi/src/memory.rs::MemoryCall`, with checked borrowed byte
buffers. Native handle IDs include generations and never expose master/data
pointers. Original Memory Manager errors(-108,-109,-112 in the accepted corpus)
are preserved in Status; `ERROR` reads the last error without resetting it.
These are native operations, not the classic 32-bit handle/pointer ABI. Limits
and original comparison boundaries are documented in the resident Memory Manager.

## Native and legacy text (development ABI 1)

All Rust Toolbox text uses UTF-8 `&str` and Rust string writers. Finder has only
`mac-toolbox` as a runtime dependency; generated resource keys resolve through
that facade. Inline native strings hold768bytes and reject oversized input
instead of silently truncating. Binary resource and fork data remains bytes.

Services carrying human-readable text have separate Pascal/MacRoman trap
identities: LOG, WINDOW, DRAW, SURFACE, FONT, RESOURCE, FILE, CONTROL, DIALOG,
SCRAP and TEXTEDIT. Their legacy number is the native number OR0x20000. Layouts are unchanged, but text length
fields count the Pascal prefix **plus** payload; an empty string is one zero
byte. Prefix/payload lengths must agree and occupy at most256bytes. Output text
uses the same convention. Raw file/resource payload reads are never converted.

System copies requests, converts legacy input to UTF-8, invokes the same native
service and converts string output back to System7 MacRoman. Malformed input,
unrepresentable output or more than255 encoded characters fails explicitly.
Borrowed raw pointer fields are restored; temporary UTF-8 storage never escapes.
The Rust facade supplies no Pascal conversion methods or Classic-call wrapper.
Original A-line traps retain their original guest-memory/stack conventions.
Byte0xDB uses the System7 currency sign U+00A4, following Apple's documented
pre8.5 mapping in [ROMAN.TXT](https://www.unicode.org/Public/MAPPINGS/VENDORS/APPLE/ROMAN.TXT).

## Dates and application versions

New native packages stamp their MacBinary Created and Modified fields with the
actual packaging time in UTC. An explicit `SOURCE_DATE_EPOCH` overrides that
time for reproducible packages; an absent value never means Unix epoch zero.
Lossless archive rebuilds preserve the archived dates. Package reports record
the chosen dates, wall-clock packaging time and optional source epoch. HFS-B image
creation preserves file dates on import, creates the volume and initial folders
at the real build time, and records its UTC creation interval for SDK validation.
Finder Get Info displays the catalog timestamps rather than a synthetic date.

Native Rust `Timestamp` holds signed64-bit Unix seconds plus a checked microsecond
fraction. Native HFS-B dates and exported manifest dates are unsigned64-bit
microseconds since1904. External classic HFS/MacBinary dates are
unsigned32-bit seconds since1904; File Manager metadata provides `created_at`,
`modified_at` and `backed_up_at` Rust timestamp accessors. Conversion is checked,
including dates in2026 and across Unix's signed32-bit boundary. The last legacy
second is2040-02-06 06:28:15UTC; later native dates remain representable and fail
legacy conversion instead of wrapping. No wall-clock or calendar/locale manager
is implied by these value and filesystem APIs. Time/date formatting belongs to
its later Toolbox milestone and must use this same conversion boundary.

Every packaged `vers` ID1 derives from Cargo's resolved package version, including
`version.workspace=true`. Build scripts use `CARGO_PKG_VERSION`; external
packaging queries Cargo metadata. Resource manifests cannot override it. Numeric
BCD versions require major<=99/minor<=9/patch<=9. Stable versions use final0x80;
dev/alpha/beta use0x20/0x40/0x60, and rc uses final with a nonzero ordinal. Unknown
prerelease labels or overflowing classic fields fail the build. The short string
retains the full Cargo version, including prerelease/build metadata.

## Resource menus, alerts and clocks (development ABI 1)

MENU0x10016 carries `mac-abi/src/menus.rs::MenuCall`. MENU/fmnu resources define
items; an optional same-ID MCMD resource supplies one big-endian command per
item. MDEF0 is an independent native code resource at0x82300000..0x82400000.
Native EventKind14 carries MenuCommand: high16 bits menuID, low16 one-based item.
Ctrl key equivalents use the same event path. A KEY query resolves an unconsumed
command-key character against the current enabled resource items without changing
menu state or repainting. The Rust selection helper uses it for queued KeyDown/
AutoKey events after the application handles preceding selection changes. This
is a native extension, not a
claim that original MenuSelect/WindowRecord contracts execute unchanged.

The native MDEF state carries explicit icon scope/ID/flags and a
presentation profile. The standard profile preserves existing menu geometry.
The System-owned application profile uses the original right-side icon title,
16-pixel command rows and18-pixel application rows; its labels come from System
STR#32760. MDEF owns all measurement, rendering and hit geometry. System owns
the live process snapshot, alphabetic ordering, foreground checkmark, hidden-icon
dimming and command policy. Applications can query its LAYOUT/COMMAND state
through menuID-16489 but cannot mutate it. Rebuild all native modules together
when the definition state changes; development ABI stays at 1.

DIALOG0x10017 carries `dialogs::DialogCall`; PSTRING_DIALOG0x30017 converts the
inline substitution at the boundary. The current alert profile loads ALRT/DITL
resources, button/static-text/ICON items, WDEF1 and CDEF0. It provides ^0
substitution, modal input routing, default Return, cancel Escape and button
press/leave/reentry/release. General DLOG/edit-text/user-items/alert stages remain
unimplemented. Unsupported resource items fail before creating a window.

CLOCK0x10018 returns signed64-bit Unix wall-clock seconds, a u32 microsecond
fraction less than1,000,000, and a zero reserved u32. The QEMU virt adapter
serializes Goldfish TIME_LOW/TIME_HIGH reads; no HFS epoch is imposed on native
Timestamp. Classic HFS writes explicitly check conversion to unsigned1904 seconds;
HFS-B writes preserve the complete microsecond value.
Resource STRING operation10 decodes STR or zero-based STR# to UTF8; only the
paired Pascal Resource trap returns a Pascal/MacRoman result.

## Native positioned events (additive native service)

POSITIONED_EVENT0x10023 uses `events::PositionedEventCall` for NEXT=0, WAIT=1,
PEEK=2, INPUT=3 and LOCATE=4. It returns the original Event plus EventPosition
(u64 complete window ID, i32 horizontal and vertical content coordinates).
Window zero means no available owned local position. INPUT can name an explicitly
owned live window for capture outside its bounds; LOCATE projects a supplied
record against current geometry. NEXT/WAIT/PEEK preserve positions captured when
native input is queued, including through owner routing and drag coalescing.
Disposal/reuse withdraws the old local identity. Differences are calculated in
32 bits; Rust checks conversion to 16-bit QuickDraw geometry.

WAIT uses the existing scheduler's mask/timeout behavior and a `waited` transport
flag followed by repoll after resumption. PEEK is non-owning and never constructs
a drag capability. Native records rebuild together at development ABI 1. The
classic 16-byte EventRecord codec remains a separate, unchanged projection.

## Monochrome TextEdit (development ABI 1, independent service)

TEXTEDIT0x1001b borrows a `TextEditCall`, optional UTF8 input and output scratch
for one synchronous request. IDs contain slot generations and require explicit
disposal. Text/selection offsets count UTF8 bytes and reject interior character
boundaries. The initial service limits are64 editors and32767 bytes per editor.
Its paired Pascal trap converts strings only; it retains native UTF8 offsets.
This is separate from original TEHandle, counted MacRoman and private TE scrap
A-line conventions. Native Copy/Cut/Paste use the shared System clipboard.
Drawing appends a clipped snapshot to the window's retained update; clients
erase and redraw inside BeginUpdate/EndUpdate after edits. Native `BLINK` (27)
services idle and XORs only the committed caret pixels, preserving retained/cache
state, occlusion and clipping; its `index` reply requests drawing only when the
caret's committed snapshot is stale. The original `IDLE` operation remains a
model-only classic-compatible call.

Native display calls use a size-distinct `TextEditDisplayCall`: GET_DISPLAY28,
SET_DISPLAY29, DISPLAY_CAPABILITIES30, DISPLAY_INFO31 and SCROLL_AXES32. Version1
options carry checked wrapping/line-number booleans and a ruler mode (0 off,
1 positive u16 columns,2 nonnegative i16 text-space pixels). Unsupported versions
or option kinds return NOT_SUPPORTED; malformed values return BAD_ARGUMENT,
without changing the editor. The extended geometry reports both scroll axes,
effective text view, logical CR-line count, gutter width, ruler x and fixed-ASCII
column advance. Existing `EditorInfo`, `LineInfo` and legacy traps are unchanged.
Defaults remain ruler off, wrap on, numbers off. Horizontal extents use wide
cached advances; legacy line widths retain their signed16-bit saturation.
Numbers count logical lines rather than wrapped rows. Gutter digit-width changes
reflow once; presentation only visits visible rows. Each edit rewraps from its
affected line to a matching unchanged line start, then shifts cached suffix
metadata. All fallible allocation is prepared before committing a splice.
See `system/src/toolbox/textedit/README.md` for the captured profile and gaps.

CRASH0x1001f is a new System RISC native service, outside every classic16-bit
trap number. `mac-abi/src/crashes.rs::CrashCall` carries BEGIN, APPEND, SHOW,
INFO, READ, SCROLL or DISMISS. Borrowed UTF-8 and output buffers are validated
at the trap boundary; READ returns its transferred byte count in `length`.
`CrashInfo` reports System-owned window/application/thread IDs, byte length,
wrapped line count, viewport position, dismissal and explicit failure flags.
Reports are scoped to the trusted calling application and retained in System,
with a1MiB limit; the shared client handler additionally logs the full formatted
stream. No original Macintosh system-error trap is repurposed. See
[panic report ownership and failure behavior](docs/PANIC-REPORTS.md).


Resident memory identities retain their original shared `toolbox::handles::Table`
wire format: high 16 bits are the nonzero generation, low 16 bits the registered
one-based slot. Memory retains slots 1..512 and all 65535 generations. Exhausted
slots retire rather than wrap. The projection's generation history is shared
across owners for the whole boot, including after zone destruction. A projected
ID resolves to a registered MMBL block and its full authenticated owner/key;
MMST owns each domain's arena, projection table and last-error state. The
registered cleanup walk releases both lifetimes, retaining backing until every
block is released. Original handle operations, errors and the 32-bit wire stay
unchanged; this is not an original 68k master-pointer ABI.

Every central 32-bit treaty namespace uses shared boot-wide generation history,
including open-file, staged-write and volume slots. Reconstructing or clearing
a table does not reset that history. Namespace selection is exhaustive; there
is no per-table generation fallback. The existing slot ranges, owner checks,
nonzero high16 generation and permanent retirement on exhaustion are unchanged.
This identity rule does not by itself establish registered lifetime/alias
coverage for the remaining File Manager objects.

Windows use registered WIND object and WNST backing lifetimes in the shared
owner-ledger facility, with opaque u64 native tokens and full-width generations.
There is no legacy window slot range, 32-window global cap or 8-bit generation
ceiling. Stale and foreign tokens fail validation before either native or
Pascal-string window service use. Both boundaries address the same manager;
this does not introduce an original WindowRecord/68k ABI.

Window creation retains an initialization owner through WDEF New/Calculate and
geometry validation. Refusal disposes the unpublished window; a failed WDEF
Dispose invalidates the token and retains the payload, provider-use pin and
unique quarantine ticket for retry. The generic owner walk uses the same release
path. WNST completion waits for every retained window payload. These are contained
refusals, not a claim of arbitrary privileged-fault recovery.

## Native managed-control identities (development ABI 1)

`ControlCall.id`, `ControlId`, `scrollbox::Call.id`, `ScrollBoxId` and retained
control/scroll-box drawing identities are opaque
u64 native tokens. A single boot-wide issuer supplies domain incarnations and
native object tokens, starting above `u32::MAX`, never wrapping or resetting.
Tokens resolve inside the shared facility to the complete owner-domain, semantic
kind, slot and u64 generation. Every lookup validates that key in the
corresponding authoritative owner ledger before accessing its value. Tokens have
no public bit-field interpretation. Remaining 32-bit service projections cannot
alias a native control token. Memory's high16/low16 contract is unchanged.

Each owner's CTRL pool grows until backing-memory refusal. It begins with one
prepared slot and doubles backing capacity on demand; this is a storage-growth
algorithm, not a control-count quota. Other apps' churn cannot spend its
slot-generation budget. An exclusive storage claim prevents raw metadata
allocation, growth or cleanup acknowledgment from bypassing the value store.
The claim lasts through outstanding removal guards. Values are destroyed outside
the owner lock before their cleanup ticket completes and their slot is reusable.
Native and Pascal-string Control calls share these IDs and resident validation;
this is not a change to an original 68k Control Manager adapter.

Managed scroll boxes use SBOX under the same native identity contract. Their
per-owner backing has a separate SBST registration; ordinary control backing
uses CSTA. These state pools contain one backing instance per domain, with an
explicit hard-bound reason. Backing does not consume an object's quota. An empty
retained cache remains ledger-owned, even after the last visible object is
disposed. The owner walk can release backing before or after its children: a
pending backing ticket is retained until the last payload leaves its storage.
Quarantined payloads retain both backing and provider pins for retry. The walk
reports callbacks accepted with a pending ticket as deferred; the final ledger
pending count, not callback success, remains the death barrier.

Standard File follows the same native identity contract: `StandardFileCall.handle`
and `DialogId` are u64 tokens in SFDL. SFST owns the package's per-domain backing
and saved active-dialog index. Completed dialogs may remain owned while a new
dialog is active; disposing an older one cannot clear the newer index. Native
and Pascal-string requests share these identities and validation. This does not
alter original 68k Standard File contracts.

All native definitions, System, apps and fixtures rebuild together at ABI 1.
The global 256-slot Control table and its shared generation array are removed.

## Native menu object ownership (development ABI 1)

MenuCall includes an opaque u64 registered handle after operation. INSTALL returns
that handle and the resource's semantic menu_id. MenuId is a resource property,
not an object lifetime. A nonzero handle on a query/mutation resolves the live
object before accessing its semantic menu number; stale and foreign handles fail.
FIND resolves a current semantic menu number explicitly, and DISPOSE requires a
live handle. CLEAR retains generation history. Native owners use these handles;
classic semantic menu operations continue to use their original resource IDs.
MENU owns each menu payload and MNST owns the per-domain ordering/tracking
backing. Both are public-ledger kinds released by the same invalidate-first walk
as explicit disposal. Menus grow to available memory; there is no global eight-menu
ceiling or packed legacy namespace. A different domain may use the same semantic
menu ID without sharing its native handle. Neither callers nor tools may infer a
slot or generation from native handle bits. The classic semantic menu operations
retain their original resource IDs and do not carry native tokens.

All native applications, System and definition resources rebuild at ABI 1.

## Native drag transport (development ABI 1)

Native Event records carry separate u64 `payload` and `window` fields; the
original big-endian 16-byte EventRecord codec excludes both. Drag notifications
put the complete generation-tagged session ID in `payload` and the complete
WindowId in `window`. These IDs must never be truncated or packed into each
other or the message field. Mouse/update/activation/geometry/close events also
carry their native window target in `window`. Trusted Window Manager close
routing captures the owner before removing that object; this routing authority
is private queue metadata and cannot be supplied by application posting. Message
high byte selects enter/within/leave/drop/promise/data-ready/finished; tracking
low bits carry item count and a precomputed intersection of offered types.
These messages are native service events, not the original Drag Manager callback
ABI. Rebuild System and every application/definition together at ABI 1.

DRAG/DRST register session payloads and per-owner backing; DRCV/DRVS register
window/desktop receivers and their backing. Existing 32-bit DragRef and receiver
namespaces project through the shared facility onto full owner-ledger keys.
Projection histories are boot-wide, including after manager destruction; an
unscoped namespace is refused by the projection constructor. The current global
wire ranges remain 32 sessions and 64 receivers. These are existing wire limits,
not an added per-application allocation quota. Payloads and saved arrays belong
to the same generic cleanup walk; no Drag Manager EXIT hook remains.

Application-file lifecycle notifications use native OsEvent message prefix
`0x0c000000` with bit 0 set while the exact file has a running instance. The
u64 payload contains mounted volume ID in its low half and CNID in its high
half. The resident process service sends this on launch/exit, independent of
activation. Finder updates only matching cached icons; lifecycle changes do not
invalidate folder windows or re-enumerate catalogs.

Native `fref` flavor payloads are exactly 32 bytes: little-endian u32 volume at
0, CNID at 4, source parent CNID at 8; little-endian i16 origin h/v at 12/14;
big-endian QuickDraw Rect at 16; file-type FourCC at 24 and a checked
little-endian u32 directory flag (0/1) at 28. A draft accepts bounded chunks
before Begin; a failed chunk poisons that draft, so no partial selection can
become visible. The parent binds a drop to the captured source snapshot,
so concurrent moves fail validation instead of moving an item from an unexpected
folder. This is distinct from the original `hfs ` HFSFlavor/FSSpec contract.

Native TEXT is UTF-8. The classic boundary converts counted MacRoman TEXT,
including classic byte sizes and partial-read/set offsets, through the same
resident data. See [the compatibility inventory](docs/DRAG-MANAGER.md).

Native ACCEPT flags are acceptance=1, Trash=2, Copy supported=4, Move supported=8.
An accepting response must name at least one supported action. Folder/Trash
admission uses the resident recursive transfer contract. Flag 16 is an explicit
same-volume-only restriction; ordinary Finder folders accept copy or move.
Application targets use flag64 with Copy only, carry exact application volume/CNID,
and may reside on read-only media. IconLayout uses flag32 for session-only
rearrangement within one immutable source folder. Unsupported actions and read-only destinations record a
rejection, without a highlight or drop delivery. Modifier changes recompute
eligibility; physical release freezes the final modifiers. Explicitly answered
rejection is separate from an unanswered offer, so release cannot wait for an
answer already received. Rebuild all native clients together for this contract.
On successful ACCEPT, `Call.index` is 1 when the response was applied and 0 when
its position/phase already expired. Ownership, thread and request validation
still reject invalid callers. The Rust result is
`TrackingResponse::{Applied, Expired}`. An expired response cannot change an
already negotiated drop; callers publish a replacement target or document batch
only after Applied. No extra query trap is needed. Target admission runs only
after the position/phase check, so an expired response performs no filesystem
admission work. On admission failure, the unchanged error Status is accompanied
by `Call.index = 2` and the exact cause in `Call.error`; the SDK exposes
`DragError::TargetUnavailable { source }`. A missing target is an error, while
only explicit resident expiry produces Expired. Foreign references and malformed
requests retain their ordinary refusals.

## Native kernel module contracts (DABI/EABI, schema 1)

A driver has Finder type `DRVR`, exactly one native `DRVR` entry resource and one
`DABI` 0 resource. An extension has Finder **file type** `INIT`. Its System code
is the native envelope in **`RV64` resource 0** with **`EABI` 0**; its Finder code
is the native envelope in **`RV64` resource 1** with **`EABI` 1**. Builders name the
code resources **System** and **Finder** by convention, but names are optional
and have no role in loader selection. A bundle may contain either target or
both; each present target has exactly one code/contract pair. System admission
requires target 0 and never substitutes target 1. Finder execution and its SDK
remain staged; package inspection alone is not execution authority.
Discovery is restricted to the selected
blessed System Folder's Drivers and Extensions directories respectively; placement
in the other directory is rejected before entry. The common module registry and
its class identifiers are independent of these discovery paths.

The entry resource uses the existing version-1 definition envelope: 40-byte NABI
with role CodeResource, minimal RV64 segment stream, then RLOC. There are no
separate NABI or RLOC resources and no ELF in a module fork. In an extension,
`RV64` names this complete **RVN1 code-resource envelope**, not the raw RV64
segment stream used by an application executable. Its reservation is
compacted to the page-rounded linked memory extent, including BSS. All module
code is relocatable. The code digest below covers the complete envelope, including
NABI, initialized segments and relocations. This detects mismatched code/contract
artifacts; it is not an authentication signature.

`mac-module` is the shared, allocation-free, safe Rust metadata reader/writer.
DABI and EABI contain the same little-endian binary layout (the surrounding
resource fork remains big-endian):

| Offset | Size | Meaning |
| --- | --- | --- |
| 0 | 4 | `KMOD` magic |
| 4 | 2 | schema, exactly 1 |
| 6 | 2 | fixed header length, exactly 128 |
| 8 | 4 | exact total resource length |
| 12 | 4 | package kind, `DRVR` or `INIT` |
| 16 | 4 | exact native kernel service ABI |
| 20 | 4 | exact generic module broker/entry ABI, 1 |
| 24 | 2 | target architecture: 1 RV64; 2 reserved for x86-64 |
| 26 | 1 | pointer width in bits |
| 27 | 1 | endian: 1 little, 2 big |
| 28 | 4 | calling convention, 1 C ABI |
| 32 | 8 | required ISA features; RV64 bits 0..3 are I/M/A/C; bit 4 requires the complete RVA23U64 profile (including V/F/D/B and mandatory Z extensions). Current module mask is 31 |
| 40 | 16 | nonzero module identity |
| 56 | 4 | native entry resource type: `DRVR` for a driver, `RV64` for an extension |
| 60 | 2 | signed entry resource ID; extension 0 selects System, 1 selects Finder; must match its EABI resource ID |
| 62 | 2 | provided interface count |
| 64 | 2 | required provider/dependency count |
| 66 | 2 | required resident service count |
| 68 | 2 | device match count |
| 70 | 2 | zero |
| 72 | 4 | declared private heap workspace in bytes (zero for forwarding modules) |
| 76 | 4 | declared callback stack budget in bytes |
| 80 | 2 | UTF-8 name length |
| 82 | 2 | UTF-8 Cargo version length |
| 84 | 2 | UTF-8 build provenance length |
| 86 | 2 | zero |
| 88 | 32 | SHA-256 of the native entry envelope |
| 120 | 8 | exact C boundary layout fingerprint |

The fingerprint encodes pointer width (bits 0..7), Host size (bits 8..23), Instance
size (bits 24..39), and Call size (bits 40..55). Bits 56 through 63 identify
full-width provider references, request tickets, device/DMA references,
backing-source references, ticket-free synchronous resident calls, resident
allocation forwarding, native module workers, and native critical sections,
respectively. Current RV64 values are 64-bit pointers and structure sizes 96, 24
and 64 bytes. Compile-time SDK assertions and the resident reject a mismatch. Changing these layouts requires rebuilding all modules; there
is no alternate old layout decoder.

After the header come the interface, dependency, service and device-match tables,
then the name, Cargo version and provenance bytes, with no padding or trailing
data. Each table has at most 32 records; the resource is at most 16 KiB. Text must
be valid UTF-8 with no control characters; byte limits are 128/64/128, and name
and version are nonempty.

An interface record is40 bytes: FourCC class at 0, exact interface version at 4,
request schema at 8, reply schema at 12, maximum request bytes at 16, maximum reply
bytes at 20, flags(u64) at 24, class-specific capabilities(u64) at 32. Maximum request
and reply lengths are 1 MiB. Flags are exclusive provider(1), serialized interface
requests(2), and kernel-only(4); unknown flags are rejected. Class, version and
schemas are nonzero; duplicate class offers are rejected. Classes are open FourCC
identifiers, not an enumeration of filesystem/block operations. A single module
may offer several independently versioned classes. Filesystem, block, directory
transport, input, network, display and future class SDKs own their request/reply
layouts and capability bits.

A dependency record is32 bytes: optional provider identity at 0 (all-zero means any
provider), class at 16, exact interface version at 20, optional flag(u32,0 or1) at 24,
and zero at 28. Class/version are nonzero. A dependency binds to a successfully
published provider, not directory ordering. Duplicate identity/class requirements
are rejected, including conflicting versions. Optional dependencies may be absent;
they do not waive validation on calls to a provider that is present.

Admission registers each present dependency as a `DDEP` claim in the dependent
module's ledger, with `DDST` backing. The claim names the exact provider and
interface incarnation, including its registered resource key. A wildcard selects
once at admission; it does not silently retarget after provider withdrawal.
Retained claims block provider/interface retirement until the dependent's
registered cleanup actually releases them. Admission stop or ordinary-ID
invalidation alone is not release acknowledgement.

A resident service requirement is16 bytes: service FourCC at 0, exact version at 4,
optional flag(u32,0 or1) at 8, zero at 12. Service/version are nonzero and duplicate
service requirements are rejected. A device match is40 bytes: bus FourCC at 0,
zero at 4,16-byte value at 8,16-byte mask at 24. A bus matches only when its class
matches and `(key & mask) == value` bytewise; value bits outside mask are invalid.
The common matcher does not assume PCI, virtio or sector-addressable storage.

mac-build generates contracts from module.json declarations, the resolved Cargo
version, current native service and broker SDK constants, target and compiler
provenance. INIT manifests select `extension_target: "system"` (default) or
`"finder"` in resources.json. This single selector determines the code/EABI IDs,
generated `NATIVE_CODE`/`NATIVE_ABI` keys and the conventional code-resource name;
package loading does not validate that name. `mac-driver::export_driver!` retains that generated declaration in a
build-time ELF section. Packaging requires an exact match to the linked declaration,
then fills the actual code-envelope digest and emits DABI/EABI. The ELF section
is build evidence; ELF section tables/headers never enter a native resource fork.
LLVM's GNU retained-section OSABI marker is accepted by the build-time extractor.

Extension startup presentation belongs to System, not executable-resource names
or author drawing callbacks. A required facility refusal leaves registrations
unpublished and records the extension's name, cause and a useful next action;
the startup strip displays an X over its package icon. In particular, Host
Clipboard requires compatible QEMU channel/protocol facilities before activation.
The EABI/code pair remains inspectable as a package after refusal; its presence
alone is not evidence of an active hook or service.

The generic resident C entry receives a borrowed Host descriptor and initializes Instance.
Host is 96 bytes on RV64. It exposes the full u64 boot-unique module incarnation,
dependency resolution, broker invocation and the callbacks below. Its descriptor
and callbacks remain immutable and live through module calls, workers,
destruction and outstanding allocations. Resolve writes a
u64 provider identity and Broker accepts that complete u64 value. The contract
fingerprint includes a provider-reference signature flag in bit56 and a native
64-bit request-ticket flag in bit57, in addition to
pointer width and structure sizes; a Host with the same size but the obsolete
32-bit provider signature is rejected through contract admission. The development
ABI remains1; rebuild System and every module together. Instance carries an opaque u64 state and a
module-local executable dispatch address, validated before publication. Rust
traits, String/Vec, allocator objects and private struct layouts never cross
this boundary. The SDK exposes borrowed slices to ordinary driver bodies.

Host also provides allocation, deallocation, replacement and a synchronous
module-allocation scope. Each callback authenticates the currently installed
module executor; the copied context number cannot select an executor or account.
The forwarding allocator is installed before driver initialization. Ordinary
allocation uses the genuine original request Subject, while explicit retained
module storage uses that module's canonical kernel account. Headers and refund
receipts stay resident; no allocator trait object, receipt or Rust container
layout crosses these allocator callbacks. Free and replacement retain the original account/class regardless of the
current caller. A refused replacement preserves its old block. A module-scope
thunk is validated against the pinned module's executable segments; it cannot
return Pending or retain its opaque stack pointer.

The bundled Host Filesystem and StuffIt Filesystem use this forwarder and declare
zero private heap workspace. Their former image-local heaps are removed. The
checked module kernel account limits actual resident backing, including headers;
a zero private-workspace declaration does not disable accounting or promise
physical memory. Callback stack declarations remain separate from the selected
OS-worker stack policy. Module-private image/DMA storage still obeys its explicit
load-range contract; forwarding does not grant arbitrary heap memory for DMA.

Module packages retain their own immutable resource source through the same
registered image facility as applications, with an explicit module role and no
application ACAP authority. Each invocation pins its real source and execution
use, selects that module's resources, and separately retains the original I/O
Subject and allocation target. Teardown waits for actual invocation readers and
non-image cleanup before releasing image aliases and final account metadata.
Physical module RAM remains reserved until reset; alias retirement alone does
not claim physical code reclamation or arbitrary privileged-fault containment.

### Native module worker and critical-section boundary

`Host`, `WorkerSpecification` and `WorkerCall` use 8-byte RV64 alignment.
`Header` is two u32 fields:
development ABI version 1 followed by the exact structure size. Every Host
callback is mandatory; entry validates the header, nonzero module incarnation,
zero reserved field and all callbacks before installing the immutable forwarding
descriptor or invoking the author's initializer. A copied context value does not
manufacture module, account or worker authority.

| Host offset | Size | Field |
| --- | --- | --- |
| 0 | 8 | Header, size 96 |
| 8 | 8 | Nonzero module incarnation, u64 |
| 16 | 8 | Reserved, zero |
| 24 | 8 | Resident callback context, u64 |
| 32 | 8 | Broker callback |
| 40 | 8 | Resolve callback |
| 48 | 8 | Allocate callback |
| 56 | 8 | Deallocate callback |
| 64 | 8 | Reallocate callback |
| 72 | 8 | Synchronous module-allocation callback |
| 80 | 8 | Workers callback |
| 88 | 8 | Critical-section callback |

The additional callback signatures are:

```rust
type Workers = unsafe extern "C" fn(context: u64, call: *mut WorkerCall) -> Status;
type CriticalSection = unsafe extern "C" fn(context: u64,
    operation: AllocationOperation, state: *mut core::ffi::c_void) -> Status;
type AllocationOperation = unsafe extern "C" fn(*mut core::ffi::c_void) -> Status;
type WorkerEntry = unsafe extern "C" fn(*mut core::ffi::c_void) -> Status;
type WorkerDestroy = unsafe extern "C" fn(*mut core::ffi::c_void);
```

`CriticalSection` authenticates the actual executing module and checks that the
thunk lies in its pinned executable segments. Native exclusion starts before
calling the thunk; nesting belongs to the actual thread context and is restored
on return. The callback is synchronous, invokes the borrowed operation exactly
once, retains neither operation nor state, and returns its nonpositive status.
Positive/deferred statuses are `BAD_ABI`.

`mac-driver::sync::Mutex<T>` uses this scope before attempting its existing
spinlock storage. `with` returns `SERVICE_BUSY` on contention without invoking
the closure; `try_with` returns `Ok(None)`. Neither spins waiting for an owner or
lends an escaping guard. Authors must keep the closure bounded. Parking and
foreign broker entry are refused while the native critical depth is nonzero;
allocation and bounded worker wake/cancel/disposal remain available. The current
native exclusion contract does not establish SMP correctness.

`WorkerSpecification` is 64 bytes:

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 8 | Header, size 64 |
| 8 | 8 | Non-null WorkerEntry function pointer |
| 16 | 8 | Non-null WorkerDestroy function pointer |
| 24 | 8 | Exclusive captured-state pointer |
| 32 | 8 | Captured-state size, usize |
| 40 | 8 | Captured-state alignment, usize |
| 48 | 8 | Borrowed UTF-8 name pointer |
| 56 | 8 | Name byte length, usize |

The name is nonempty, at most 128 bytes, and contains no control characters.
Entry and destroy thunks must lie in this module's pinned executable ranges.
The unsafe native contract requires exclusive `Send + 'static` captured state
with the exact declared layout, allocated by the resident forwarder. A
zero-sized state uses a non-null, correctly aligned dangling pointer. Dimension,
alignment and callback-range checks do not establish arbitrary pointer safety
or privileged-code isolation.

The safe `mac-driver-alloc::Workers` extension trait provides
`Client::prepare_worker(name, body)` for a
`FnOnce(WorkerContext) -> mac_driver::Result<()> + Send + 'static` body. Capture
storage uses `ForwardingAllocator` explicitly under the module's canonical
Kernel allocation scope, even if the author selected another global allocator.
The SDK reserves before moving the body. Failed preparation returns its typed
cause and the original body; all fallible resident admission precedes the
single publication handoff. Successful CREATE transfers ownership irrevocably
and returns the published token. It cannot later report a preparation failure
and let the SDK destroy a capture already owned by the resident.

The specification and name are borrowed only through CREATE return. After
publication the resident retains the real module execution/source pin and
registered thread cleanup. Exactly one path consumes the captured allocation:
entry runs the body and its destructors, or destroy disposes an unstarted body.
Both use the matching resident allocator. Callbacks and destructors run outside
registry/scheduler/owner locks; refused cleanup retains the payload, code pin and
unique ticket for retry. Uncooperative privileged code is not force-unwound.

`WorkerCall` is 48 bytes:

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 8 | Header, size 48 |
| 8 | 4 | Operation, u32 |
| 12 | 4 | Flags, u32; zero on input |
| 16 | 8 | Generation-tagged THRD worker token, u64 |
| 24 | 8 | Borrowed WorkerSpecification pointer for CREATE; null otherwise |
| 32 | 4 | Worker state, u32; STATUS result |
| 36 | 4 | Exit status, i32; actual result when FINISHED |
| 40 | 8 | Reserved, zero |

STATUS and CURRENT report sticky cancellation with output flag
`CANCEL_REQUESTED=1`; no other flag bits are defined. Cancellation is independent
of a finished body's actual exit status. States are PREPARED=0, ARMED=1,
RUNNING=2, PARKED=3 and FINISHED=4.

| Operation | Value | Boundary and behavior |
| --- | --- | --- |
| CREATE | 0 | Host; worker input zero, specification present; returns the published token |
| START | 1 | Host; arms during initialization, or starts after Ready |
| WAKE | 2 | Host; coalesces a wake for this worker |
| CANCEL | 3 | Host; sticky cooperative cancellation, waking a parked worker |
| STATUS | 4 | Host; returns current state, cancellation flag and finished result |
| DISPOSE | 5 | Host; records cancellation and eventual token disposal without waiting |
| CURRENT | 6 | Host; worker input zero; returns the actual worker token and cancellation flag |
| PARK | 7 | Private native trap; consumes a pending wake or sleeps atomically |
| EXIT | 8 | Private native trap; scheduler trampoline exits after entry returned |

Host controls authenticate the actual module and retain separate THRD ownership
and generation checks. Starting an initializing module only arms the worker;
allocation-free activation follows successful registry publication. Failed
initialization cannot start an armed body. STATUS/CURRENT do not grant authority
to a foreign module. DISPOSE of a valid token retains the cancellation/disposal
intent; actual destruction and removal happen through registered cleanup.

The private RV64 trap number is `0x4d57524b` (`MWRK`): `a7` carries the number,
`a0` the WorkerCall pointer and `a1` its exact 48-byte extent; Status returns in
`a0`. Only PARK/EXIT use this switch boundary. The trap requires the actual
module-worker body at the outermost trap depth, pre-trap interrupts enabled and
no active critical section. A nonzero token must be that current worker's token;
zero selects the actual current worker. A wake recorded before PARK is consumed
without sleeping, and wakes coalesce. Sticky cancellation returns
`OPERATION_CANCELLED` (-17). EXIT is accepted only after the resident has recorded
entry return; author code cannot use it to skip capture destruction. Process-only
Toolbox traps reject worker contexts; diagnostic LOG remains available.

The SDK's unique `PreparedWorker::start` returns a Worker or a typed failure
retaining its preparation. Worker cancellation and `try_join` do not wait;
`try_join` distinguishes pending execution from a finished body's result. Drop
requests DISPOSE and diagnoses unexpected refusals while the resident retains
cleanup ownership. A copied WorkerWaker retains only a stale-checked token, with
no account/code/stack reference after retirement. WorkerContext is constructed
only by the entry shim and is neither Clone nor Send. Exited stack/CPU backing
can be reclaimed independently of the small retained result token. These
contracts do not claim physical module-image reclamation, arbitrary privileged
fault recovery or full extension-service readiness.

Call is 64 bytes and carries a full-width u64 native resident ticket, phase, class, class operation,
borrowed input and exclusive output bytes. START allocates a ticket; POLL and
CANCEL validate the same caller/provider/request incarnation. QUIESCE has no
class or buffers. Each callback returns a terminal status/length or PENDING(1).
Terminal completion clears Call's ticket to zero. Pending and pre-dispatch
contention retain the live ticket, so a caller can distinguish an unfinished
operation from a terminal provider error. A callback receives its live ticket;
the broker clears it only after validating and retiring the request.
Pending callbacks retain no buffer pointers. They must do bounded work and
return; the interface does not authorize waiting on hardware with interrupts
masked. Current broker polling is caller-driven; deferred I/O scheduling and
class-specific cancellation/progress gates accompany the storage providers.

The resident records trusted call provenance independently of this wire. An
active callback installs its genuine request receipt in the I/O subject frame;
nested direct admissions validate that active parent, executor and root scope.
An owned queue starts its own frame. Module entry keeps one I/O root across
Start/Poll/Cancel. None of these authorities is accepted from mutable Call fields.

Failed entry withdraws publication and starts the ordinary ledger walk. Only
abandoned direct roots enter resident cancellation; nested protocols and owned
queue jobs retain their original coordinators. Direct-root reply capacity and
return-queue storage are reserved before publication. Pending cancellation keeps
its exact receipt and scope. A real terminal reply ends serialized occupancy;
nonempty opaque results remain pinned for class-specific reconciliation rather
than discarding already committed effects. Uncertain invocation outcomes retain
original reply storage and never manufacture acknowledgement. Physical module
unload and arbitrary privileged-fault recovery remain separate gates.

### Resident device service (VDEV, version 1)

Resident class providers use the same generated interface/request capabilities
as independently loaded modules. Their callback route uses reserved module0,
while the actual interface registration and storage belong to the genuine System
owner ledger; zero is not an issued resource owner. A module declares resident classes in `services`, not module
`dependencies`; resolution and every request check that declaration separately.
Only an admitted DRVR with a matching bus key can claim physical hardware.

`mac-device-interface` owns this class's byte layouts; `mac-driver::devices`
provides the Rust binding. These payloads do not alter the generic C entry ABI.
All fields are little-endian. Description is 52 bytes: native claim alias u64
(zero for probe) at 0, bus class four bytes at 8, 16-byte match key at 12,
register base u64 at 28, register length u64 at 36, interrupt u32 at 44, flags
u32 at 48. Flags are coherent DMA=1, MMIO=2, resident-owned=4. Mapping is
24 bytes: native mapping ID u64 at 0, device address u64 at 8, length u64 at 16.
Nonzero handles must be outside the classic 32-bit projection space. Invalid
lengths, overflow and unknown flag bits fail. The module layout fingerprint
includes the full-width device contract (bit 58); development ABI stays 1.
Stale packages must be rebuilt, rather than decoded through a second layout.

| Op | Input | Output |
| --- | --- | --- |
| 0 Probe | bus ordinal u32 | 52-byte description, no claim |
| 1 Claim | bus ordinal u32 | 52-byte description with owned claim alias |
| 2 Map | claim u64, module RAM address u64, length u64 | 24-byte mapping |
| 3 Reset | START: claim u64; POLL/CANCEL: empty | empty; Pending until stopped acknowledgement |
| 4 Unmap | mapping u64 | empty; refused while DMA can still run |
| 5 Release | claim u64 | empty; requires no mappings/pending reset and stopped device |
| 6 Count | empty | inventory slot count u32; slots may be empty |

Physical targets belong to the System domain (`DVOB`/`DVOS`); modules receive
facility-owned aliases (`DVAL`/`DVAB`), never global target IDs. DMA mappings
(`DMAP`/`DMST`) belong to the actual module domain and retain the exact claim's
admission receipt. All six kinds use the public registration catalog and common
invalidate-first owner walk. An invalidated claim remains exclusive until its
mappings and hardware stop settle, regardless of callback ordering.

Reset polling/cancellation uses only the original broker-issued ticket and
owner. Ordinary stale aliases remain invalid. Cleanup submits one successful
reset per claim, probes once per retry, and retains the original ticket, payload
and backing on failed writes, failed probes or absent stop acknowledgement.
Mapping cleanup precedes last-claim reclamation by an explicit count, rather
than reliance on pool ordering. A positive stop permits retiring a dead claim;
a late reset callback cannot reset or bind a replacement claim. Losing the
manager quarantines its claims, maps and System targets; destruction is never
a hardware acknowledgement. No operation spins waiting for hardware.

RAM mappings are restricted to the caller's pinned native module reservation.
The current QEMU virt bus uses coherent identity DMA; this is not physical-page
pinning, IOMMU isolation, module-image reclamation or multi-hart qualification.

The board bus class is `VMMI`; its match key is device ID u32, vendor ID u32,
then eight zero bytes. Probe ordinals 0..7 describe the board's eight 4 KiB
virtio-MMIO windows. Boot storage and input stay resident-owned until their
explicit extraction milestones; declaring a matching DABI key cannot steal
them. Claim publication re-probes the exact authorized descriptor.

Initialization is staged. No interface is visible until entry and callback-address
validation succeed and every interface is registered transactionally. Registry
locks and File Manager guards are released before any module callback. New
requests are withdrawn before quiescing; pending requests prevent finalization
until completed/cancelled. Module, interface and request identities use the shared
generation-tagged facility, including rejection of foreign IDs and slot reuse.
Code/host descriptors remain pinned after failure; hot unloading is not implemented.
The admission profile fixes separate stored-code (2 MiB), runtime reservation
(6 MiB), declared heap (4 MiB) and callback-stack (64 KiB) limits in mac-module.
The packager/loader and resident registry use the same constants. In particular,
a BSS-backed decoder workspace does not count as stored executable bytes.
Alignment and each exhausted budget have distinct diagnostic errors. The machine
adapter still admits modules only while their complete reservations fit its
actual remaining arena. These checks are admission limits, not an MMU or quota.

The checked QEMU virt platform reserves `0x87800000..0x88000000` (8 MiB) for
modules, separately from the ROM's live storage scratch heap at
`0x87000000..0x87800000`. Its default RAM remains 8 GiB; the checked physical map
requires at least 256 MiB so this reservation and the high 2 MiB device-tree tail
are disjoint. Generated boot reservations, pager constants and linker symbols
use this same record. Page metadata starts after the module reservation.
The image builder sums complete installed System-module descriptor spans,
including BSS/private heaps, and refuses an over-capacity installation with its
package name and required bytes. Runtime admission independently reports the
remaining reservation and requested extent. Code capacity does not grant a
module worker, heap account or stack budget. These addresses belong to the
machine adapter, not the class-neutral registry.

## Driver-owned file associations (FASS 0, version 1)

A native module can include one `FASS` resource, ID 0. `mac-module` owns its
allocation-free codec. The resource is big endian, with an eight-byte header:
magic `FAS1`, u16 version 1, u16 rule count (1..32), then exact 12-byte records:

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | Exact Finder file-type code, ASCII graphic bytes |
| 4 | 2 | Action: 1 = mount filesystem |
| 6 | 2 | Signed icon family ID in this module's fork; 0 = no default icon |
| 8 | 4 | Reserved zero |

Duplicate file codes are rejected, including conflicts with another
initializing/ready module. File-name classification is a separate preference
service; FASS contains no extension matching rules.
Mount actions require a driver offering FSYS v4 `SOURCE_FILE`. Local icon families
are decoded once at admission and shared while painting; they do not reserve
global System icon IDs or read resource forks during repaint.

Declarations publish atomically when the module becomes Ready and stop matching
on quiesce/failure. An absent driver supplies neither an open action nor an icon.
Current module code stays pinned until reboot: deleting its file is not hot unload.
`Files::open_item` returns `None` for an unclaimed document and `Some(VolumeInfo)`
after an acknowledged mount; explicit `mount_file` returns NotSupported if unclaimed.
Finder uses this generic service, with no archive-type or suffix special case.

### File type preferences

`System Folder/Preferences/File Types` is a UTF-8 JSON document with exactly
`version` (currently 1) and `associations`. Each association has `extension`,
`file_type`, and `creator` fields. Extensions are case-insensitive ASCII
alphanumeric suffixes of 1..16 bytes without the dot; duplicate extensions are
invalid. Codes are exactly four printable ASCII bytes. The document is bounded
to 64 KiB and 128 associations. The initial image associates `sit` with `SIT!`
type and creator codes. The resident service loads the preference at boot;
malformed or unsupported preferences report a specific startup error.

Classification projects zero/BINA file metadata without changing stored bytes or
overriding explicit types such as TEXT or APPL. This grants no launch capability:
only an admitted, Ready driver supplies the matching action and default icon.
Removing that driver leaves classification intact but removes its action/icon.

## Backing-file service (FORK v1)

Filesystem providers with `SOURCE_FILE` (FSYS capability bit 1) receive a
resident source capability in Mount's auxiliary field. `Files::mount_file`
selects this class using a generation-checked mounted volume and persistent file
CNID. A source belongs to the admitted provider module incarnation and identifies
the original `(volume generation, CNID, fork)`; repeated mounting reuses the live
mount, while copied files have distinct identities. No host path is supplied.
File-backed Mount also carries the source file's current FSYS Metadata as its
payload (not a host path). This supplies its actual UTF-8 name and timestamps
for the mounted root. Host-export Mount continues to have no payload.

The driver declares the kernel-only `FORK` service, version 1. All fields are
little endian. Both operations use an exact 24-byte initial argument:

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 8 | native resident source capability (u64, outside classic projection space) |
| 8 | 8 | logical fork offset |
| 16 | 4 | requested read length, at most 65536 |
| 20 | 4 | reserved zero |

Info (operation 1) requires zero offset/length and returns an eight-byte u64 fork
length through the ticket-free synchronous resident phase. It performs no
backing I/O and cannot defer; contention is safe to retry without a hidden
request. Read (2) uses START and returns positional fork bytes, short at EOF,
without changing any public file mark. Overflowing ranges and offsets beyond
EOF fail. READ POLL/CANCEL carry empty input and retain the same broker ticket
and source lease; borrowed input/output storage never survives a call.

Sources (`FSRC`) and their backing (`FSSB`) are registered native module-owned
leased-fork capabilities; the global file/catalog remains System-owned. Ordinary
IDs are generation-validated independently of module ownership. Native u64
source tokens flow through Mount auxiliary, FORK and the SDK. Module layout
fingerprint bit 59 rejects old packages; development ABI remains 1. Both producer
and consumer rebuild, with no older-source decoder.

Read and close plans carry their original publication receipt and owned backing.
Losing a plan quarantines its protocol state; a plain ID cannot manufacture
completion authority. A refused completion returns its original plan. Actual
backend ACK may settle a retained plan after owner invalidation. Cleanup queues
intent while its registry context is borrowed, then drives one owned close
continuation outside that borrow. Storage for those continuations is reserved
before source publication.

Backing marks belong to the selected module's admitted I/O scope, including
positional reads and post-stop close. Provider acquisition replaces its known
unopened Catalog placeholder before Open; it never changes a published mark's
owner. The exact lower provider is pinned by a scoped registered dependency.
Only the unique root's actual closure removes that claim; metadata lookup failure
is not evidence of closure. Scope-close notifications run after metadata unlock
and coalesce module dependency cleanup, with no scan on ordinary input events.

Close queue admission pins the validated file mark across fallible allocation;
refusal leaves it unchanged. A terminal close error permits a close-only retry,
while reads remain refused. Temporary local retirement refusal retains the real
terminal reply rather than polling a consumed job again. Source close is owned
by resident File Manager/common-cleanup actors after the requesting app exits.

Sources and requests retain separate module ownership checks. A source pins its
backing file and volume against mutation and unmount. Publication follows
acknowledged acquisition. Unmount/cancellation
cannot recycle it before provider teardown and backing Close acknowledge;
uncertain cleanup quarantines the binding. Block-backed and provider-backed files
use the same source identities. Provider acquisition (Catalog/Open), positional
reads, and Close run as owned deferred jobs, with no File Manager or registry
guard across callbacks. Pending read/cancel storage survives until acknowledgement;
an uncertain read quarantines its source instead of making it reusable. A
serialized filesystem cannot mount its own files recursively, because that
requires reentrant provider instances; this currently returns NotSupported.

`mac-file-source-interface` owns this codec and `mac-driver::file_sources` the
borrowed Rust client. The private `file_sources` public-SDK gate loads a test FSYS
DRVR, mounts real HFS-B backing files, checks large positional reads, retained
pins, distinct copies, repeated mounts, failed-mount cleanup and stale IDs after
slot reuse. Its provider also verifies that a source remains live inside Unmount
and fails after acknowledgement. This is not a production StuffIt decoder.

## Native host fork containers (RSFORK01, version 1)

`mac-fork-file` is the safe no_std reader/writer used by host tools and the native
host filesystem provider. The whole-number container version fixes both byte
layout and Unicode 17.0.0: names are stored NFC, at most 255  UTF-8 bytes, and
compared using the same canonical caseless rules as HFS-B 1. Changing that contract
requires a new complete format version and rebuilding affected representations;
it is not an independent Unicode field.

Each regular guest file is one host file containing native metadata, both forks
and chunk integrity hashes. Directories remain host directories. The fixed header
is256 bytes, followed by UTF-8 name and comment bytes and zero padding to a fixed
1024-byte metadata area. Fork data starts at 1024 regardless of name length, so
metadata-only renames do not shift fork data. All integers are little-endian.

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 8 | ASCII `RSFORK01` |
| 8 | 4 | whole format version, 1 |
| 12 | 4 | header length, 256 |
| 16 | 4 | kind: file 1, directory 2, root 3 |
| 20 | 4 | backup-present flag,0 or 1 |
| 24 | 4 | persistent catalog ID |
| 28 | 4 | root's next unused catalog ID; zero for other kinds |
| 32 | 16 | nonzero persistent object identity |
| 48 | 16 | nonzero export/volume identity |
| 64 | 8 | nonzero container revision |
| 72 | 8 | created, unsigned microseconds since 1904 |
| 80 | 8 | modified, same epoch/precision |
| 88 | 8 | backup date; zero when absent; present-zero means1904 |
| 96 | 32 | complete Finder information, including file type/creator |
| 128 | 4 | catalog attributes in low16  bits; high16 zero |
| 132 | 2 | UTF-8 name byte length, 1..255 |
| 134 | 2 | UTF-8 comment byte length, 0..255 |
| 136 | 8 | logical data-fork length |
| 144 | 8 | logical resource-fork length |
| 152 | 8 | data-fork physical offset, 1024 |
| 160 | 8 | first data-chunk hash offset |
| 168 | 8 | resource-fork physical offset |
| 176 | 8 | first resource-chunk hash offset |
| 184 | 8 | exact complete host-file length |
| 192 | 4 | logical chunk size, 4096 |
| 196 | 4 | zero |
| 200 | 32 | SHA-256 of the complete header with this field zero, then name/comment bytes |
| 232 | 24 | zero |

A fork is a sequence of up-to4096  logical bytes followed immediately by that
chunk's 32-byte SHA-256, repeated until its declared logical length is reached.
There is no padding between chunks or forks and no hash for an empty fork.
Interleaving hashes keeps sequential I/O sequential. A fork occupies
`length + ceil(length/4096)*32` physical bytes; an empty fork's first-hash offset
is its start. Header offsets and total length must agree exactly with these
formulas, with checked 64-bit arithmetic. Extra/truncated bytes, unsupported
versions, nonzero reserved/padding bytes and checksum failures are rejected.
These are integrity checks, not authentication signatures.

Directories/root records cannot contain forks. Root has catalog ID2, object
identity equal to volume identity, and next catalog ID at least 16. Other entries
have IDs at least 16 and next-ID zero. A child's identity starts as its volume
identity, with the final little-endian 32-bit word increased by its catalog ID
modulo 2^32. If that produces sixteen zero bytes, byte zero is set to 0x80.
This mapping is injective within the volume and distinct from its root identity;
a child's metadata must contain exactly this derived identity. Persistent
catalog IDs and identities are
file metadata, not runtime handles. Runtime mount/open/request leases use the
shared generation facility. The cooperating writer advances the root counter
before publishing a new entry and never reuses IDs, including after a failed
publication. Revision/counter exhaustion fails explicitly.

Host components are the actual case-preserving NFC names, limited to 255 UTF-8
bytes. The physical directory entry is authoritative for a child's current name;
embedded catalog/object identity survives a host rename and a remount. The
embedded name is a metadata snapshot, not a second naming namespace. Guest
creation rejects nonportable Windows components before reserving an ID or writing:
control characters, `<>:"/\|?*`, trailing dot/space, DOS device names (including
extension variants), and the case-insensitive `.system-risc-` bookkeeping prefix.
There is no opaque identity-to-filename encoding. Other mounted filesystems retain
their own naming contracts.

Root metadata lives in `.system-risc-root`; each native directory has
`.system-risc-directory`. A root-only `.system-risc-lock` serializes cooperating
transactions. These records are hidden from guest directory listings. Ordinary
unwrapped host files expose their exact bytes as a data fork and an empty resource
fork, without conversion or sidecars. Native container kind/volume mismatches,
duplicate catalog/object identities and canonical name collisions are errors,
never silently merged or ignored. Importing a copy assigns fresh identities and
catalog IDs; copying a raw container does not.

Writers use unpublished, exclusively created `.system-risc-stage-*` files. The
shared chunk writer writes a valid header last, requires every fork chunk before
finish, and reports flush errors. The host adapter syncs staged file contents,
publishes new files without replacement, atomically replaces existing metadata
records, and syncs the containing directory. Stages are cleaned on ordinary
failure; a leftover crash stage is reported as an unfinished transaction. There
is no claim that a recursive import is one atomic transaction. A failed import
may leave completed entries and skipped IDs; it never reports success for an
unfinished entry. Unsupported durability primitives refuse writable operations.

Readers own their leased source together with decoded metadata/layout. Source
adapters validate revisions before/after operations; changed in-place sources
return Stale rather than mixing revisions. Atomic replacement may leave a prior
leased inode as a consistent old snapshot. The host tool uses capability-relative
paths, rejects symlink entries and checks global identity/name consistency.

The `risc-forks` Linux tool provides init/import/import-macbinary/export/list/
mkdir/check. Its lossless `system-risc-forks` JSON+fork bundles interoperate with
the HFS-B tool, preserving both forks, complete Finder information, comments,
attributes, and unsigned 64-bit microsecond dates. Copying assigns destination
identities; IDs/revisions are not restored as another live object's identity.

## Host directory wire protocol (RSHOST04, version 4)

`mac-host-protocol` owns the allocation-free codec and incremental reply decoder.
The VM byte channel carries a 52-byte little-endian header followed by exactly
`payload_length` bytes (maximum 65536). There are no native pointers, Rust enum
layouts or implicit host paths on the wire. Unsupported versions require a
matching rebuild; there is no older decoder.

| Offset | Bytes | Meaning |
| --- | --- | --- |
| 0 | 8 | `RSHOST04` |
| 8 | 2 | whole protocol version, 4 |
| 10 | 2 | header length, 52 |
| 12 | 4 | payload length, at most 65536 |
| 16 | 8 | nonzero u64 correlation ticket, echoed unchanged in reply |
| 24 | 2 | operation below |
| 26 | 2 | 0 request, 1 reply; other bits invalid |
| 28 | 4 | session capability; zero only during Hello |
| 32 | 4 | object capability |
| 36 | 8 | operation-specific offset |
| 44 | 4 | operation-specific value |
| 48 | 4 | signed status; requests must contain zero |

The guest broker's complete native request ticket supplies correlation; it
confers no object authority. The companion issues session/file/directory/iterator/
transaction capabilities through the shared object table. Owners are validated
separately. Disconnect discards the old session; a new transport incarnation
must invalidate every old guest mount/lease before admitting new host IDs.

A path is one UTF-8 component of 1..255 bytes. Dot components, control characters,
slash, backslash and colon are invalid. The filesystem's opaque identity tokens
fit this representation. Guest names and Macintosh metadata are encoded in the
separate fork-container format, never inferred by this transport.

| Op | Request | Successful reply |
| --- | --- | --- |
| 1 Hello | session/object/offset/value zero; empty body | new session; value=export count |
| 2 Export | object zero; offset=export ordinal | object=directory capability, value=capability bits, body=export label |
| 3 OpenDirectory | object=parent, body=component | object=directory capability |
| 4 ListBegin | object=directory | object=iterator capability |
| 5 ListNext | object=iterator | value=1 regular file or 2 directory, body=component; value=0/empty at end |
| 6 OpenRead | object=parent, body=component | object=snapshot, offset=length, body=32-byte opaque revision |
| 7 Read | object=snapshot, offset=position, value=maximum bytes | body=bytes up to EOF |
| 8 Close | object=capability | terminal; transaction cancellation removes its unpublished stage |
| 9 BeginWrite | object=parent | object=unpublished transaction capability |
| 10 Write | object=transaction, offset=position, body=bytes | value=written byte count |
| 11 SetLength | object=transaction, offset=new length | empty |
| 12 Flush | object=transaction | empty, file sync acknowledged |
| 13 Commit | object=transaction, value=expected snapshot or zero for new, body=destination component | empty, file publication and directory sync acknowledged; transaction consumed |
| 14 MakeDirectory | object=parent, body=component | empty, containing directory synced |
| 15 Rename | object=source directory, offset=destination directory capability, value=expected source snapshot/directory, body=two names | empty, no destination replacement; directories synced |
| 16 Remove | object=parent, value=expected source snapshot/directory, body=component | empty; only empty directories removable; parent synced |
| 17 SyncDirectory | object=directory | empty, sync acknowledged |
| 18 OpenLock | object=parent, body=existing regular lock-file component | object=lock capability |
| 19 Lock | object=lock, value=0 shared or 1 exclusive | empty; nonblocking, Busy on contention |
| 20 Unlock | object=lock | empty |
| 21 Stat | object=snapshot/directory | value=1 file or 2 directory, offset=file length, body=32-byte opaque revision |
| 22 Describe | object=parent, body=component | object=catalog identity, value=1 file or 2 directory, offset=file length, body=88-byte description below |
| 23 PublishDirectory | object=flushed transaction, body=two names: target, metadata-record name | object=new directory capability; containing directory synced; transaction consumed |
| 24 DescribeObject | object=owned open directory, read snapshot, stage or lock; other fields zero | same 88-byte description as Describe, with the checked pinned object's identity/revision |
| 25 BeginRename | object=source directory, offset=destination directory, value=expected source snapshot/directory, body=two names | object=owned rename intent; no publication |
| 26 FinishRename | object=rename intent | publish without replacement and sync both directories; repeated calls reassert durability without a second rename |
| 27 RenameState | object=rename intent | value=0 unpublished or 1 published; does not publish; uncertain source substitution is an error |


Unused request fields must be zero. Rename encodes `u16 first_name_length`, first
name, then second name; PublishDirectory uses the same two-name encoding.
Describe identities come from the shared generation table and are scoped to the
session and export. A pinned host object preserves identity across rename while
preventing inode reuse from aliasing a deleted object. Host creation dates use
birth time where supported, otherwise modification time; all epoch conversions
are checked. Describe grants no read or directory capability.

The description is exactly 88 bytes: created/modified u64 Mac-epoch microseconds
at 0/8, u32 incarnation-present at 16 (0/1), reserved zero u32 at 20, 32 opaque
incarnation bytes at 24, and 32 opaque current-revision bytes at 56. Absent
incarnations require all-zero identity bytes; present identities must be nonzero.
The qualified Unix profile encodes device, inode, signed Unix birth seconds and
birth nanoseconds in the incarnation, with four trailing reserved zero bytes.
Providers treat these bytes as opaque. Missing birth support is explicitly absent,
never a path/mtime/inode-only substitute for persistent identity. Revisions change
on writes even when mtime is restored; they are distinct from object incarnation.
Generation-tagged session IDs still authorize runtime lookups, independently of
these persistent backing keys. Unsupported old wire/class versions require a
matching rebuild of System, both drivers and the host companion; no v1 path is
retained. FSYS v4 and the RSFORK01 storage format are unchanged by this wire change.
Enumerations stop with Stale if their directory changes.
Only currently active transaction stages are hidden; leftover crash stages remain
visible for explicit format validation/recovery. Read-only exports refuse every
mutation and exclusive lock. Export capability bits are Read=1, Write=2,
Durable=4, AtomicPublish=8; Linux read/write exports qualify all four at startup.

Status zero is success. Negative values are Invalid=-1, Stale=-2,
Unavailable=-3, ReadOnly=-4, Conflict=-5, Limit=-6, IO=-7, NotFound=-8,
Busy=-9, Unsupported=-10. Errors contain no body or object/offset/value result.
Malformed framing closes the connection. Correlation/direction/operation or
storage-bound failures poison the guest incremental receiver; partial replies
cannot become a different live request. A partial header/body is never an EOF
success.

See `tools/risc-host/README.md` for staged-publication, cooperating-lock and
external-edit limits. This protocol is a directory class, not a block protocol;
actual block exports require their own class contract.

### Host directory transport class (`HDIR`, version 4)

The loadable Host Transport driver publishes the serialized, exclusive `HDIR`
class. This is a byte/directory transport, not a block device and not a Macintosh
catalog implementation. The separate filesystem provider consumes it. Operations
use the `RSHOST04` operation numbers above. START input is a 24-byte little-endian
argument record followed by its exact payload:

| Offset | Bytes | Meaning |
| --- | --- | --- |
| 0 | 4 | Host session capability; zero only for Hello |
| 4 | 4 | Host object capability |
| 8 | 8 | Byte offset |
| 16 | 4 | Operation value |
| 20 | 4 | Exact following payload length, at most 65,536 |
| 24 | variable | Payload |

The broker's live request ticket supplies wire correlation; callers cannot choose
it. POLL/CANCEL have empty inputs. A completed output is the full checked 52-byte
`RSHOST04` reply header followed by its exact payload. Transport errors and host
operation status are distinct: a completed transport can carry a failed host
operation. Each callback transfers at most two 4 KiB fragments in each direction,
plus bounded device control work. Packet buffers belong to the driver, never to
an application or its borrowed call frame. Cancellation drains an already-sent
operation and reports its actual result; it does not promise to undo publication.
A lost established connection or malformed reply poisons that driver incarnation.

The current RV64 adapter claims only a declared modern virtio-console device via
`VDEV`. Four pinned queues implement data RX/TX and multiport control RX/TX. DMA
mappings remain live until a positively acknowledged reset; failed initialization
cannot reclaim DMA-visible module memory. The module remains pinned until reboot.

## Native filesystem provider class

The `FSYS` class version 4 carries bounded little-endian owned byte requests and
catalog/mount replies, independent of resource-fork layouts and physical storage.
The exact header/metadata fields and allocated operation numbers are documented
in [FILESYSTEM-INTERFACE.md](docs/FILESYSTEM-INTERFACE.md); their shared checked
codec lives in `crates/mac-filesystem-interface`. This adds a class contract and
does not change the generic module entry boundary or the native code envelope.
Archive failures use the negative status namespace defined in
`mac-abi::archive::ArchiveError`: -65536 through -65542 identify truncation,
signature, header CRC, fork CRC, malformed data, configured limits and encryption;
-65792 minus the external version byte and -66048 minus the compression-method
byte retain those unsupported-feature parameters. -66304 minus the entry-header
version byte identifies an unsupported record layout separately from the archive
container version. These are terminal error
identities, not transport failures. Preserve them through job completion and
File Manager cleanup; no thread-local last-error buffer is involved. The SDK
returns `Error::Archive`, and Finder formats resource-based explanations with
the retained byte. Unknown statuses remain lossless.

Catalog/Lookup flag bit 0 requests an exact child count. Metadata-only ancestry
queries leave unscanned counts unknown instead of enumerating unused children;
all other flag bits are rejected. Header/metadata schemas remain 1; producers
and consumers rebuild together, with no older project decoder. Class v3 creation
uses the documented FileProperties template, and data-only replacements can
request verified resource-fork preservation. Wrong installed contract versions
produce an ABI/rebuild error; physical HFS-B/host-container formats are unchanged.
BeginCreate/BeginReplace return prospective metadata. FinishWrite returns the
documented WriteReceipt envelope containing the published file and Mount
metadata; its storage is reserved before publication, including cancellation
cleanup that observes a successful commit. No post-commit allocation or extra
catalog query is needed to report the actual result.
AbortWrite reports an unpublished boolean or the complete observed publication
receipt. Sequential write payloads are at most 64 KiB, aligned to 4096 bytes
except at the declared fork end. A failed batch must be aborted rather than
blindly replayed; providers may have acknowledged a bounded prefix.
Native FileCall operations 26, 27 and 28 are host-export count, mount host export
(index selector), and unmount volume. All other input fields must be zero except
the volume for unmount. Mounted identities are issued by resident File Manager,
remain hidden until validated publication, and retire only after acknowledgement
or cancellation known not to have reached the provider. The public SDK names are
`host_export_count`, `mount_host_export` and `unmount`.
Native allocation-info and directory-scan outputs reserve `u64::MAX` for an
unknown physical fork allocation. The Rust SDK exposes Option<u64>; known zero
remains Some(0). HFS/HFS-B block-address limits exclude this reserved value.
Native catalog child counts similarly reserve u32::MAX for an unknown count,
exposed as Option<u32> in the SDK, including unscanned provider folders.
Provider directory cursors bind the volume generation, catalog revision and
directory; unused cursor fields are zero. Index advances only for returned
entries. Pending returns control to userspace, and Done is terminal for that
snapshot. Invalid scope/revision/progress is rejected, not silently restarted.

### Directory change notifications

Native FileCall operations 31/32/33 register, poll, and dispose a watch. Register
supplies volume/cnid and returns a generation-tagged handle; poll/dispose supply
only that handle. Poll returns index 0/1 for a coalesced change and position for
the new catalog revision. Unused inputs are zero. Poll consumes a change once,
performs no storage I/O, and enforces the same owner checks as disposal. A pending
probe retains its disposed mark and volume lease until terminal acknowledgement.

FSYS mount capability `CHANGES=4` advertises external monitoring independently of
`WRITE=2`. Operation 21 CheckDirectory supplies volume/node with no flags/payload;
its 33-byte reply is a changed byte (0/1) followed by 32 opaque revision bytes.
The provider invalidates only that directory's cached descendants when its
revision changes. It performs no enumeration in a cached-directory probe.
Immutable backends accept resident watches without physical probes.

The Linux companion mixes a bounded inotify generation into directory Stat
revisions, covering child content/metadata edits as well as name changes.
Each nonblocking read processes at most 4 KiB of notifications; overflow also
changes the revision. The monitor attaches to a pinned directory capability and
is independent of guest write permissions. Access/open/read notifications do not
invalidate the catalog. This notification backend currently requires Linux.

## Retained window update acknowledgement

Native WINDOW operation 11 (`VALIDATE_WINDOW`) acknowledges an owned window's
currently pending update damage while keeping its retained contents. It performs
no drawing, allocation, scene invalidation or presentation. An active drawing
transaction returns SERVICE_BUSY and preserves its damage; a stale/foreign
identity is rejected through the Window Manager's generation/ownership checks.
Repeated acknowledgement is a no-op. New invalidations after acknowledgement
remain observable. `Windows::validate` exposes this to Rust applications.
Finder uses it while rebuilding an unpublished folder snapshot: otherwise an
unacknowledged synthetic Update can starve idle scan work indefinitely. The
completed refresh still publishes only its actual changed item bounds.

## Application-defined standard window bounds

Native WINDOW operation 12 (`SET_STANDARD_BOUNDS`) sets an owned window's standard
zoom rectangle. `reserved=0` applies `bounds`; `reserved=1` clears the proposal
and restores the System's default desktop zoom. Other reserved values, rectangles
smaller than the window minimum, or structures outside the usable desktop return
BAD_ARGUMENT. A stale/foreign identity follows normal generation/owner validation.
The proposal performs no resize, invalidation, event generation or repaint;
identical proposals are no-ops. System owns button tracking/cancellation, saves
the user's bounds on entering standard state, and restores them on the next click.
GET_WINDOW returns current zoom state as 0/1 in `reserved`. The SDK exposes
`WindowInfo::zoomed` and `Windows::set_standard_bounds` / the owned-window method.

Finder derives its proposal from the complete cached icon-and-caption extent,
chrome and minimum size, constrained to the usable screen. Zoom restoration also
restores its user scroll origin. Manual resizing establishes a new user state.

## Optional host annotations and metadata patches

`mac-fork-file::annotation` owns the fixed 512-byte `RSMETA01` record used in
`.system-risc-metadata/<64 lowercase hex incarnation bytes>`. It is optional
bookkeeping; ordinary host file bytes and user-visible names remain untouched.
Native fork containers retain their existing `RSFORK01` framing.

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 8 | `RSMETA01` |
| 8 | 4 | LE whole format version 1 |
| 12 | 4 | LE record bytes, 512 |
| 16 | 16 | Export volume identity |
| 32 | 32 | Qualified host object incarnation |
| 64 | 4 | LE persistent catalog ID (2/root or 16..0x7fffffff) |
| 68 | 2 | LE encoded patch length |
| 70 | 2 | Reserved zero |
| 72 | 8 | LE annotation revision; zero only for an identity-only empty patch |
| 80 | 16 | Reserved zero |
| 96 | variable | MetadataPatch, then zero padding through byte 479 |
| 480 | 32 | SHA-256 of bytes 0..480 |

Reject unknown versions with a recreate/reimport diagnostic. Reject checksum,
reserved-bit, padding, identity or volume mismatches rather than guessing at
metadata. The source incarnation is independent of editable guest dates.

`mac-filesystem-interface::MetadataPatch` uses a 104-byte LE header followed by
an optional UTF8 comment (at most 255 bytes): schema/header length u16 at 0/2,
presence flags u32 at 4, attribute mask/value u16 at 8/10, comment length u16 at
12, zero u16 at 14, created/modified/backup microseconds u64 at 16/24/32,
32-byte Finder mask at 40 and 32-byte Finder value at 72. Presence bits are
comment=1, created=2, modified=4, backup-field=8 and backup-present=16.
Unselected scalar values are zero; value bits outside their masks are invalid.
Absent, empty, cleared and explicit zero remain distinct.

FSYS v4 SetMetadata(20) takes volume/node/resident writer token/when plus the
patch. Rename(16) takes volume/node/token/when plus a UTF8 name; Move(17) takes
volume/node/token/new parent/when with no payload. Their preparation replies
start with LE u32 0 (unchanged) or 1 (staged), then prospective Metadata. Only a
staged result is followed by FinishWrite/AbortWrite. MetadataReceipt permits the
root only when its file metadata equals the accompanying mount root; ordinary
creation/data-write receipts still prohibit root publication.

Native FileCall SET_FOLDER_STATE(37) saves DInfo.frRect/frView and DXInfo.frScroll.
Only input entry.finder_info bytes 0..8 and 14..20 are used; other Finder bytes
must be zero. It preserves placement, flags, comments and forks. The typed Rust
interface is Files::set_folder_state[_at] with FolderState. Finder coalesces
geometry/scroll changes before persistence.

Native drag response flag TARGET_ICON_LAYOUT(32) describes presentation-only
rearrangement. It requires Move alone, no Trash bit, and the same (volume,parent)
for every frozen inline file reference. It cannot authorize filesystem mutation
or make a read-only destination writable. Classic drag/file semantics are
unchanged; this flag is the native Finder view-state path.

Native drag response flag TARGET_EJECT(128) describes a volume-to-Trash eject,
not a filesystem transfer. It requires Move alone, root CNID 2, and exactly one
frozen inline directory reference whose volume matches the destination. It
cannot combine with application, Trash, copy or icon-layout intent. Eject is
always Move regardless of modifiers and accepts read-only mounts. Resident
admission rejects the startup volume, unsupported block-device unmounts, active
forks/writers/transfers, executable/document pins and outstanding provider reads;
the unmount service repeats those checks before entering the driver. The
tracking-summary bit 2 identifies a single volume-root reference and is computed
once at Begin, so ordinary pointer motion does not inspect reference payloads.
Finder saves pending folder state before unmounting and closes that volume's
windows only after the provider acknowledges teardown. Failure preserves them.
Mounted volume IDs retire through the shared generation-tagged object table.

Root SetMetadata publishes the volume record itself; its confirmed modification
date is authoritative. View-state edits preserve that date, and explicit date
restoration is never overwritten with the transaction's operation timestamp.


### Bounded native fork writers

`ForkWriteCall` is a size-distinct native File Manager record, exposed by the
Rust SDK as `ForkWriter`. It declares u64 data/resource lengths and retains
native UTF-8 creation metadata and comment. BEGIN returns a generation-tagged
FileStreams identity; APPEND, READ, FINISH, ABORT and ABANDON validate that identity
and its application owner. Each payload is at most 64 KiB. Nonfinal append
chunks are multiples of 4096 bytes; each fork advances independently. READ
verifies unpublished contents without exposing a staging CNID.

FINISH/ABORT are stepped: completed=0 requires another bounded call,
completed=1 retires the mark. Abort distinguishes an unpublished operation from
an already-published file. The SDK owner requests resident cleanup on Drop;
async finish/abort use ordinary core futures and work with a std executor.
Application exit requests the same cleanup. An uncertain rollback retains its
owner and quarantines the affected storage rather than returning false success.

HFS-B stages ordinary catalog files in the invisible, reserved root directory
`.System RISC Transactions`, whose comment identifies the staging contract.
Every payload block remains reachable from a valid catalog entry while copying.
Publication changes both forks and all logical metadata in one transaction.
Replacement preserves its target CNID and swaps the old payload into the hidden
stage; reclamation frees at most 64 KiB per step. Exclusive startup recovery
reclaims unpublished/old stages with the same stepped operation. The physical
HFS-B v1 layout and Unicode contract remain unchanged; these are ordinary files,
not orphan extents or a second filesystem implementation. Classic HFS explicitly
rejects admission to this new staging service.

FSYS v4 operation ReadWrite(22) reads verified bytes from an owned unpublished
writer: volume/token, fork flag, offset and length, without publication.
BeginReplace flag REPLACE_PROPERTIES=2 carries a FileProperties template;
PRESERVE_RESOURCE_FORK=1 remains the distinct data-only save intent. Existing
resource preservation and full-fork replacement are deliberate operations.
The host wire and HDIR v4 expose ReadWrite(28) for positioned reads of an owned
unpublished transaction; ordinary read snapshots remain immutable. Obsolete
wire/interface versions require rebuilding both guest packages and host tooling.


Host NotifyDirectory(29) invalidates a folder’s logical metadata revision without
changing its physical timestamps. Watches are shared by pinned directory
incarnation across aliases/exports, including read-only aliases. Private
`.system-risc-*` bookkeeping does not count as a directory-content event;
metadata publication explicitly notifies the affected parent once. Pure saved
folder-window geometry does not notify or re-enumerate its contents. Directory
revision polling uses the shared event counter and pinned identity, with no
per-poll fstat and no enumeration.


## Host removal receipts and recovery

HDIR/RSHOST v4 operations 30–34 are BeginRemove, QuarantineRemove,
CommitRemove, AbortRemove and RemoveState. BeginRemove uses the source directory
in object and the expected source capability in value. Its payload is a LE u16
source-component length, source bytes, then an optional approved private marker
component. It returns a shared-facility transaction capability. The other four
operations use that capability with no other input. RemoveState values are
prepared=0, quarantined=1, committing=2, removed=3, aborted=4; AbortRemove reports
whether irreversible removal had already committed. Close acknowledges cleanup.
A filename alone never authorizes deletion; foreign or stale capabilities fail.

A `.system-risc-delete-<nonce>` journal beside the source records the exact
qualified parent/source/marker incarnations, original component and private
quarantine component. It is exclusively locked while live. Two 1024-byte slots
use the same format, with the highest valid sequence selected after a torn write:

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 8 | `RSHDEL01` |
| 8 | 4 | LE whole format version 1 |
| 12 | 4 | LE phase (0, 1, 2 or 4 above) |
| 16 | 32 | Qualified parent incarnation |
| 48 | 32 | Qualified source incarnation |
| 80 | 32 | Qualified marker incarnation, zero when absent |
| 112 | 6 | Three LE u16 component lengths: original, hidden, marker |
| 118 | 2 | Reserved zero |
| 120 | 8 | LE monotonic slot sequence |
| 128 | variable | Original, hidden and optional marker UTF-8 components |
| after names | variable | Zero padding through byte 991 |
| 992 | 32 | SHA-256 of bytes 0..992 |

Each component is validated/bounded to 255 bytes. The hidden component must have
its reserved stage prefix. The prepared intent is durable before quarantine;
the committing phase is durable before irreversible deletion. Recovery restores
precommit quarantines without replacing a colliding visible object. Committed
cleanup addresses only the recorded hidden incarnation, never a reused visible
name. Corrupt/foreign/locked intents retain their objects and report a typed
error. Enumeration triggers recovery when it encounters private leftovers, then
invalidates its old snapshot; ordinary enumeration performs no recovery scan.

FSYS Remove admission returns tag + original Metadata. FinishWrite/AbortWrite
return RemovalReceipt (LE u32 actual removed flag followed by Mount), since the
removed catalog entry no longer exists. Kernel and SDK ownership lasts through
acknowledged cleanup, including cancellation and application exit.


## Native document delivery and transfers (development ABI 1)

Size-distinct `DocumentCall` records use the Process trap. Native handles and
`DocumentBatchId` are opaque u64 identities; no classic projection or fixed
sixteen-batch quota applies. Begin reserves a registered owner-bound batch before
declaration I/O. Classic `open` resources supply
creator-checked accepted FourCCs; compatibility probes load only those payloads,
not CODE. Push validates and pins each exact volume/CNID; Commit validates the
complete set against the executable actually launched and queues it to that
exact file instance. Commit issues a distinct receiver-owned batch ID, admits
its catalog aliases before notification, then invalidates the sender ID. A
refused handoff keeps the original sender batch intact. Queued/receiving owners
retain references and mount pins until Finish or process cleanup. Probing/preparing owners abort through RAII;
a drag-bound batch is also abandoned on drag cancellation. Native OsEvent
`0x10000000` announces queued documents, with the full u64 batch ID in payload.

DBAT/DBST register batches and per-owner backing. Catalog pins use FPIN/FPST
process aliases of FPOB/FPOS System targets. Document scopes are owned, private
lifetime objects; alias leases cannot close them. On cancellation/owner death,
ordinary batch IDs invalidate immediately but lower I/O and catalog references
remain retained until actual completion. Scoped alias cleanup uses preallocated
linear removal guards and is ordinary deferral, not a failed cleanup. Releasing
a public ID is never acknowledgement that its file is unpinned. Declaration
I/O completion wakes the cleanup pass; physical input does not rescan it.

Size-distinct `FileTransferCall` records use the File trap, including four
reserved zero u32 words. File record sizes are compile-time checked for
uniqueness. `FileTransferCall`, `DataWriteCall`, `ForkWriteCall` and
`IconGroupCall` carry full native u64 object identities; genuine classic fork
references retain their documented 32-bit treaty. Transfer notices carry that
full identity in the event payload. Native-only write/stream/group/transfer
projection namespaces are removed; their registered object pools grow until
memory refusal. The shared transfer
ID owns bounded planning, hidden staging, two-fork copying/readback verification,
source quarantine, grouped root publication and reverse-order undo. Copy, Move
and per-source-volume Trash share the coordinator. Cancellation and the
irreversible commit decision are serialized together. `commit_decided` is an
explicit checked boolean; progress phase alone cannot imply publication.
Phases are Preparing=0, Copying=1, Verifying=2, Committing=3, RollingBack=4,
CleaningUp=5, Committed=6, Cancelled=7, Failed=8, Reading=9 and Writing=10.
Reading/Writing describe presentation progress, not new publication barriers.
Cleanup failures retain their leases/ledger and report the original and cleanup
causes separately. Coalesced OsEvent `0x0f000000` notices carry the transfer ID;
observing progress acknowledges its pending notice. Drop/app exit request owned
cancellation without abandoning actual storage receipts. Native public-SDK
acceptance covers both endpoint disconnects, cancellation, Drop and application
exit, recursive two-fork transfers, rollback and immediate healthy-volume reuse.
These gates do not claim exhaustive hardware fault injection.

Host transport packets have a five-second no-progress deadline measured by the
machine monotonic clock, including while cancellation drains a sent request.
Connection loss, malformed/foreign replies and timeout poison that incarnation;
late bytes cannot be interpreted as another request's receipt. Unknown storage
publication remains owned/quarantined for acknowledged recovery. A dead endpoint
does not retain leases on untouched volumes; after destination undo completes,
unrelated healthy storage is available even if a read capability still needs
cleanup on the unreachable endpoint.

Native TextEdit snapshots distinguish `content_revision` (only changed text)
from presentation revision. Native ScrollTo/RevealSelection operations use
32-bit pixel offsets and report `scroll_y`/`scroll_max`, leaving classic QuickDraw
rectangles intact. Painting and selection geometry visit only visible lines.
Text Edit keeps its document newline style (CR/LF/CRLF), preserves resource forks
on data saves, and uses System-owned Open/Save As/Save Changes dialogs.

Preferences are versioned JSON documents with Finder type `JSON`, creator `TEDT`
and extensionless native names. The initial registry is `Preferences/File Types`;
Text Edit uses `Preferences/Text Edit Preferences` with schema version1,
`font_family` and `font_size`. Its default is Monaco12. JSON/TEXT are declared
Text Edit document types; font/view settings never modify document contents.


Text Edit preferences use strict JSON version2 with `font_family`, `font_size`,
`wrap`, `line_numbers`, `ruler_kind` and `ruler_value`. Unknown fields, invalid
values and earlier schemas are rejected; recreate preferences with a rebuilt
installation. The extensionless preference document retains JSON/TEDT metadata.
Normal release builds omit input/state logging. `text-edit/input-trace` is an
explicit diagnostic feature; `cargo xtask check` enables it only in private check
media (alongside resident tracing). Neither diagnostic is required for normal use.

## Native dynamic menu and control presentation

MenuCall carries an inline MenuItemState replacement. Development ABI stays at
1 until the first proper release. Rebuild System, applications, drivers,
definitions and guest fixtures together when this record changes. Trap
boundaries require the current record size. SET_ITEM (9) replaces
one existing item or appends exactly at count + 1; TRUNCATE (10) only shrinks.
System validates UTF-8, bounds and the live menu handle, measures a staged state
through MDEF, then publishes. Failed requests preserve the previous menu.

ControlDefinitionState uses its former reserved i16 as flags. DEFAULT_BUTTON
(bit 0) is valid only for push buttons. The declared bounds contain the classic
three-pixel default outline, a white gap, and the button inset four pixels.
CDEF0 owns drawing and hit geometry; the outline does not activate the button.
Other flags and use on non-button controls are rejected.

SCROLL_BOX ROW_AT (10) returns the resident row at a window-local point in row,
or u32::MAX for blank space, partial trailing rows and the scrollbar. It uses
the current scroll position and the same generation/owner validation as other
scroll-box operations. It does not invalidate or paint.

## Application authority policy (ACAP 0, development ABI 1)

The System resource `ACAP` ID 0 is generated by xtask from the trusted build
policy `system/application-authority.json`, after every application is packaged.
It grants authority to exact native resource content. Application names, paths,
creator codes and self-declared resources do not authorize inspection. Copies of
the same authorized content have the same grant; modified content does not.

All policy fields are big endian:

| Offset | Bytes | Meaning |
|---|---|---|
| 0 | 4 | Magic `ACAP` |
| 4 | 4 | Whole format version, 1 |
| 8 | 4 | Grant count |
| 12 + 40*n | 32 | SHA-256 canonical resource-content identity |
| 44 + 40*n | 8 | Capability mask; bit 0 grants cross-domain inspection |

The exact length is `12 + 40*count`. Empty/unknown capability masks, unknown
versions, truncation and trailing bytes are errors. Validate the entire policy
before returning a grant. No match (or no policy) grants no cross-domain
permissions; ordinary own-domain inspection remains available. This policy does
not gate public exec-chain/handler-claim/kind-mapping registry enumeration and
does not yet grant termination.

Canonical identity hashes `System RISC application identity v1\0`, followed by
u64 resource count, then resources sorted by `(type bytes, signed i16 ID)`.
Each record is: type[4], i16 ID, u8 attributes, u8 optional-name-present flag,
u64 name byte length, raw name bytes, u64 data length, data bytes. Every integer
is big endian; duplicate keys are rejected. The shared `mac-formats::authority`
codec is the build tool and resident loader's single implementation. Resource
fork offsets, container timestamps and ordering do not affect identity.

Native trap `INSPECTION` (0x10024) authenticates the current process on every
call. BEGIN returns exact independent process/module directory membership revisions,
a combined count and explicit coverage flags. DOMAIN carries both revisions;
changes in either included directory return
SCAN_STALE with a restart explanation. Each row is a locked ledger snapshot
with a separate revision and cumulative successful births/deaths. Each row
includes both live objects and invalidated objects whose cleanup is pending.
Invalidation revokes an ID immediately; its slot remains unavailable until the
unique cleanup ticket completes. A domain cannot retire while either count is
nonzero. Refused completion retains the ticket for retry against its proper
owner; it never acknowledges cleanup implicitly. Counters and
revisions never wrap; saturated revisions refuse snapshots. Coverage includes process and module domains with partial ledgers; System
and bootstrap domains and unmigrated objects remain outside this census.
Snapshot values allocate no resident session object and convey no authority to a
foreign caller. Coalesced OsEvent 0x11000000 advises process/module membership changes. This tag is
distinct from the Window Manager geometry notification.
THEME (0x10025) returns a resident role-palette snapshot; applications cache its
revision rather than embedding palette choices.


## Native TextEdit identities (development ABI 1)

EditorId and all editor references are opaque u64 tokens from the shared owner
store. TEDT registers editor payloads (including cursor geometry) and TESV owns
the per-domain backing. The old 64-editor table and u16 editor generation array
are removed; editor count grows until fallible backing admission fails. There is
no public bit interpretation. Native and Pascal-string services validate the
same live object; this does not introduce an original 68k TEHandle layout.
Rebuild System, applications, definitions and fixtures together; the development
ABI stays 1. Native font, edit and display request sizes remain distinct and are
checked before borrowing. The existing 32767-byte text engine limit remains
until the TE2 storage stage.

## Native resource Alert lifetimes (development ABI 1)

ALRT registers the classic Alert protocol; ALST registers per-app backing.
The existing one-active-Alert-per-app rule remains explicit. The public DialogCall
still names the Alert's full-width WindowId, validated against the current live
protocol, not a resource number or reusable index. Window close and crash-report
replacement invalidate the protocol too. Buttons parsed from DITL become managed
CDEF controls with their own lifetimes; the default ring belongs to the button's
four-pixel-expanded bounds. Reserved call fields and overflowing ring bounds are
rejected before publication. This does not add an original DialogRecord layout.


Native MDEF measurement treats title/popup/item bounds as output geometry;
non-geometry menu fields are input. Menu disposal stages those outputs in
prepared registered backing and commits them only after every measurement
succeeds. A measurement that changes input is rejected as BAD_ABI without
invalidating the menu or publishing a partial reflow. Disposal allocates no new
measurement workspaces or geometry vectors.

## Native System mapping inspection (development ABI 1)

INSPECTION operations 7 (BEGIN) and 8 (READ) use the separate
`mac_abi::inspection::memory::Call` payload; all other operations retain their
lifetime-inspection payload. The binding first checks the operation header,
then validates the complete selected payload size/alignment. Reserved fields
must be zero. System addresses are cross-domain detail and require the live
caller's `INSPECT_DOMAINS` policy grant on every request. Public registry
existence/enumeration remains unprivileged.

BEGIN returns a root revision, page size, discovered RAM bytes, current free
pool pages, live heap used/capacity, actual root physical address and region
count. Current coverage explicitly says shared supervisor mappings and
unavailable per-domain accounting/reserve budgets. Heap and page counters are
backend samples, not a simultaneous machine-wide snapshot. READ accepts that
revision and a row index and returns at most 16 rows plus the next index and
HAS_NEXT flag. Sixteen is a transport batch, not an inventory/capacity limit.
Changed revisions fail SCAN_STALE with a restart explanation; invalid indices
fail NOT_FOUND. Snapshot values are data, never object IDs or authority.

Each row carries virtual base, physical base, byte length, R/W/X/user/global
protection and semantic kind. Unmapped stack-guard rows carry no protection and
physical zero; a mapped physical address of zero remains valid. Direct-map rows
are aliases, not additional physical charges. Code, read-only data, writable
data, actual heap backing, System stack, temporary trusted application/module
arenas and devices are distinguished. The native source walks the actual owned
immutable root, caches contiguous matching spans once on explicit request and
validates unmapped guards before publication. It does not report planned
mappings as installed mappings. Failed allocation leaves initialization
retryable. Private process maps and per-domain budget gauges remain later gates.


## Resident Scrap extension service (SCEX, version 1)

SCEX is a kernel-only synchronous resident service for declared INIT modules.
The broker authenticates the actual module; the service requires a committed
extension and a live module ledger, then validates every generation-tagged
session and snapshot against that owner. Labels and expected content revisions
confer no authority. Applications use the ordinary Toolbox Scrap calls.

Every operation uses the ticket-free synchronous broker phase and borrows fresh
buffers. The caller supplies 512..4096 reply bytes. Before any effect, System
validates the declared input and reply bounds. A terminal reply has an eight-byte
little-endian header: status i32 and payload length u32. Status zero carries the
operation result. A negative status carries a nonempty UTF-8 diagnostic from the
original typed cause. Broker contention before dispatch has no effect or hidden
ticket and is safe to retry. No independently linked callback runs under Scrap.

| Operation | Input | Success payload |
| --- | --- | --- |
| OPEN (1) | writable u32, exactly 0 or 1 | session u64 |
| NOTICE (2) | session u64 | latest pending content revision u64; 0 means unchanged |
| SNAPSHOT (3) | session u64 | immutable snapshot pin u64 |
| READ (4) | session u64, pin u64, byte offset u64 | up to 4088 text bytes |
| IMPORT (5) | session u64, pin u64, UTF-8 text | committed content revision u64 |
| CLOSE_PIN (6) | session u64, pin u64 | empty |
| DISCONNECT (7) | session u64 | empty |
| INFO (8) | session u64, pin u64 | revision u64, content kind u32, text length u32 |

INFO kind 0 is absent/empty Scrap, 1 is UTF-8 text including an empty string, and 2 is
unsupported content. Non-text kinds have length zero. READ pins one immutable
revision across chunks. Successful commits replace one pending revision per
session without allocating notification queues. Snapshot allocation failure
preserves both its pending notice and the previous pins. The scheduled observer
chooses the final batching boundary; no clear/put end marker is invented.

IMPORT accepts at most 1 MiB minus 32 text bytes and also applies the resident
Scrap bound. It checks the exact live writable session, its snapshot pin and the
current content revision before replacement. A conflict, storage failure or bad
input preserves the previous Scrap. The origin session acknowledges only its
committed import; other observers retain the notice. DISCONNECT revokes its pins.
Owner cleanup retains original quarantine tickets through acknowledged release.
The contract does not itself admit a worker, schedule observer callbacks or
qualify host clipboard interoperability.


## Resident allocation/account coverage

Domain account coverage1 means execution backing plus attributed resident heap.
Each native resident allocation privately retains its original account/class and
exact backend Layout, including header/alignment cost. A surviving admitted I/O
root can continue within that original budget after ordinary owner stop; closed
roots/accounts cannot supply new authority. Realloc preserves original account,
class and refunds, including when a later genuine same-account root supplies
continuation after an earlier root closes. Account observations do
not include independent application/module heaps or executable-image physical
pages. The ordinary heap is480 MiB within one512 MiB backing; protected32 MiB is
excluded. Foundation/System/domain totals are categories of that same ordinary
storage, not additive physical capacities. The checked platform module account
policy selects16 MiB kernel admission and a4 MiB future worker stack; it does not
activate workers. Rebuild all consumers together when changing these contracts.

The System memory-inspection summary separates
`ordinary_foundation_heap_bytes` from `tracked_foundation_physical_bytes`.
The former is a category within ordinary heap occupancy; the latter attributes
the actual rounded page-pool leases held by prepared CPU service stacks, entry
controls, saved CPU arrays, affine System idle executors and their raw directory
backing. Each unique
physical owner carries one move-only receipt until backing disposal. Mapping
aliases and guard addresses do not issue receipts, and physical attribution
does not reduce ordinary heap capacity or subtract pool pages a second time.

This physical counter has partial coverage. Primary and prepared peer idle
executors now carry their actual stack and SavedState receipts. Existing page
tables, page-pool metadata, ROM reservations and protected backing are excluded. Protected
Foundation heap attribution remains a separate origin. The summary retains
`ACCOUNTING_UNAVAILABLE`; these two observations do not claim an all-System
memory total or a synthetic System domain account. Mapping revision does not
claim that heap, physical attribution and page-pool counters were sampled
simultaneously. ABI remains version1; the native layout fingerprint changes
with the summary layout and all producers/consumers rebuild together.
