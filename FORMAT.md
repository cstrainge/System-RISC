# Native resource executable format, version 1

The separate native disk contract is documented in
[HFS-B on-disk format, version 1](docs/HFS-B-FORMAT.md). Its format version also
fixes its Unicode name contract; changing those tables requires rebuilding every
affected file/name/index representation, not changing an independent setting.

The container is a classic Macintosh file. Its resource fork uses the standard
big-endian resource header, data area, map, type/reference lists and Pascal names.
The data fork is currently empty. Host transport uses MacBinary II with a valid
CRC-16/XMODEM header and 128-byte fork padding. File/resource names use Mac Roman.

Native executables use an `'RV64'` resource ID 0 and a companion `'NABI'`
resource ID 0. The unreleased format is **version 1**. ELF is a build-time input,
never a runtime resource container. The old unreleased ELF-backed layout is
replaced, not supported by a second decoder. Rebuild ROM, System, Finder,
applications and definitions together when this contract changes.

The [unreleased compatibility policy](ARCHITECTURE.md#unreleased-compatibility-policy)
applies to all native formats, ABIs and schemas. Update every producer/consumer
and regenerate project artifacts when changing a layout; maintaining previous
System RISC layouts, automatic migrations or fallback decoders is not required.
Keep validation and clear rebuild/recreate diagnostics for incompatible input.

## NABI descriptor

NABI has exactly 40 bytes. All numeric fields are big endian:

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | Magic `RVN1` |
| 4 | 2 | Format version: 1 |
| 6 | 2 | Role: 1 = resident System, 2 = application, 3 = code resource |
| 8 | 4 | Native System service ABI version: 6 |
| 12 | 4 | Reserved, must be zero |
| 16 | 8 | Inclusive preferred load reservation base |
| 24 | 8 | Exclusive preferred load reservation limit |
| 32 | 8 | Linked entry point |

The role, ABI and reservation meanings are unchanged. Entry is explicit because
there is no ELF header at runtime. It must be even and lie inside initialized
executable segment bytes. Versions, reserved fields, lengths, address overflows
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

The build still links RV64IMAC ELF64 System V `ET_EXEC` binaries, validates them
with `elf::decode`, and extracts only initialized PT_LOAD bytes. Dynamic linking,
interpreters, TLS, unsupported machine/ABI flags, bad alignment and overlapping
ELF segments fail packaging. Link relaxation remains disabled. Full debug ELFs
and stripped host artifacts can remain in build output; none is stored in a
runtime code resource. The segment stream is the intended output shape for a
future 68k translator too; implementing that translator is outside this change.

## RLOC and native definitions

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
ABI 7's `SystemServices` is 24 bytes on RV64:

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 8 | ABI version 6 and structure size |
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
runs in M-mode with shared memory and without memory isolation. Payload pointers must obey their
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

The current BootInfo is88bytes on RV64; SystemServices remains24bytes. BootInfo
contains RAM/log information, firmware disk-info/read/write/flush callbacks,
startup disk/partition identity and flags. Device ordinals remain stable while
System runs; NOT_FOUND ends enumeration. Each read/write transfers exactly512
bytes; flush is an explicit durability barrier. Callback contexts and DMA queues
remain firmware-owned and are serialized by the resident owner guard. DiskInfo
is24bytes and carries sector count, read-only state and driver icon identity.
Missing write/flush callbacks make storage read-only. Unknown native artifacts
must be rebuilt together; no earlier BootInfo layout is decoded.

Native catalog and volume names have256-byte UTF8 buffers, enough for255-byte
NFC names plus a spare byte. Catalog dates are unsigned64-bit microseconds since
1904, with a separate backup-presence flag so the actual epoch is representable.
VolumeInfo carries filesystem kind and whole filesystem version (HFS has no
explicit disk version; HFS-B currently reports1) and an explicit blessed-folder
CNID; HFS-B does not overload the Finder-info bytes to carry its blessing.
CatalogInfo and VolumeInfo are
each368bytes; FileCall is832bytes on RV64. Native clock results carry signed Unix
seconds and a checked microsecond fraction; classic seconds projections are
explicit and reject dates outside their32-bit range.

Window and retained drawing titles accept768 UTF8 bytes, enough for all255
MacRoman characters after conversion. Legacy Pascal/MacRoman conversion remains
at the trap boundary; native application interfaces expose Rust strings.

FILE requests and metadata are defined in mac-abi/src/files.rs. File IDs are
opaque mounted-volume IDs plus persistent catalog IDs; volume and open handles include
a generation. Reads copy into caller storage, and both forks have independent
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

## Native and legacy text (ABI4)

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

## Resource menus, alerts and clocks (ABI5)

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

ABI6 extends the native MDEF state with explicit icon scope/ID/flags and a
presentation profile. The standard profile preserves existing menu geometry.
The System-owned application profile uses the original right-side icon title,
16-pixel command rows and18-pixel application rows; its labels come from System
STR#32760. MDEF owns all measurement, rendering and hit geometry. System owns
the live process snapshot, alphabetic ordering, foreground checkmark, hidden-icon
dimming and command policy. Applications can query its LAYOUT/COMMAND state
through menuID-16489 but cannot mutate it. Rebuild all native modules together
for ABI6; ABI5 definition-state layouts are incompatible.

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

## Monochrome TextEdit (ABI5 independent service)

TEXTEDIT0x1001b borrows a `TextEditCall`, optional UTF8 input and output scratch
for one synchronous request. IDs contain slot generations and require explicit
disposal. Text/selection offsets count UTF8 bytes and reject interior character
boundaries. The initial service limits are64 editors and32767 bytes per editor.
Its paired Pascal trap converts strings only; it retains native UTF8 offsets.
This is separate from original TEHandle, counted MacRoman and private TE scrap
A-line conventions. Native Copy/Cut/Paste use the shared System clipboard.
Drawing appends a clipped snapshot to the window's retained update; clients
erase and redraw inside BeginUpdate/EndUpdate after edits or caret transitions.
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


Resident memory and window identities use the shared `toolbox::handles::Table`.
The high16 bits hold a nonzero generation; the low16 bits hold a registered,
one-based slot. Memory retains slots1..512 and all65535 generations, exactly as
before. Windows use the disjoint range513..544 and generations1..255 so their
IDs remain representable in the existing24-bit close-event payload. These are
opaque service-scoped IDs; applications must not infer allocation order or
construct IDs from slot numbers. Removal/clear invalidates the old generation;
an exhausted slot is permanently retired rather than wrapped. Existing owner
checks and Pascal/68k adapter contracts are unchanged. Other resident managers
must register and adopt this facility when their implementation is next touched.

## Native menu object ownership (ABI 7)

MenuCall includes a u32 generation-tagged handle after operation. INSTALL returns
that handle and the resource's semantic menu_id. MenuId is a resource property,
not an object lifetime. A nonzero handle on a query/mutation resolves the live
object before accessing its semantic menu number; stale and foreign handles fail.
FIND resolves a current semantic menu number explicitly, and DISPOSE requires a
live handle. CLEAR retains generation history. Native owners use these handles;
classic semantic menu operations continue to use their original resource IDs.
All native applications, System and definition resources rebuild at ABI 7.

## Native kernel module contracts (DABI/EABI, schema 1)

A driver has Finder type `DRVR`, exactly one native `DRVR` entry resource and one
`DABI` 0 resource. A System extension has Finder type `INIT`, exactly one native
`INIT` entry and one `EABI` 0 resource. Discovery is restricted to the selected
blessed System Folder's Drivers and Extensions directories respectively; placement
in the other directory is rejected before entry. The common module registry and
its class identifiers are independent of these discovery paths.

The entry resource uses the existing version-1 definition envelope: 40-byte NABI
with role CodeResource, minimal RV64 segment stream, then RLOC. There are no
separate NABI/RV64/RLOC resources and no ELF in a module fork. Its reservation is
compacted to the page-rounded linked memory extent, including BSS. All module
code is relocatable. The code digest below covers the complete envelope, including
NABI, initialized segments and relocations. This detects mismatched code/contract
artifacts; it is not an authentication signature.

`mac-module` is the shared, allocation-free, safe Rust metadata reader/writer.
DABI and EABI contain the same little-endian binary layout (the surrounding
classic resource fork remains big-endian):

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
| 32 | 8 | required ISA features; RV64 bits 0..3 are I/M/A/C |
| 40 | 16 | nonzero module identity |
| 56 | 4 | native entry resource type, matching package kind |
| 60 | 2 | signed entry resource ID |
| 62 | 2 | provided interface count |
| 64 | 2 | required provider/dependency count |
| 66 | 2 | required resident service count |
| 68 | 2 | device match count |
| 70 | 2 | zero |
| 72 | 4 | declared heap budget in bytes |
| 76 | 4 | declared callback stack budget in bytes |
| 80 | 2 | UTF-8 name length |
| 82 | 2 | UTF-8 Cargo version length |
| 84 | 2 | UTF-8 build provenance length |
| 86 | 2 | zero |
| 88 | 32 | SHA-256 of the native entry envelope |
| 120 | 8 | exact C boundary layout fingerprint |

The fingerprint encodes pointer width (bits 0..7), Host size (bits 8..23), Instance
size (bits 24..39), and Call size (bits 40..55); bits 56..63 are zero. Current RV64
sizes are 64,40,24,56 respectively. Compile-time SDK assertions and the resident
reject a mismatch. Changing these layouts requires rebuilding all modules; there
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

A resident service requirement is16 bytes: service FourCC at 0, exact version at 4,
optional flag(u32,0 or1) at 8, zero at 12. Service/version are nonzero and duplicate
service requirements are rejected. A device match is40 bytes: bus FourCC at 0,
zero at 4,16-byte value at 8,16-byte mask at 24. A bus matches only when its class
matches and `(key & mask) == value` bytewise; value bits outside mask are invalid.
The common matcher does not assume PCI, virtio or sector-addressable storage.

mac-build generates contracts from module.json declarations, the resolved Cargo
version, current native service and broker SDK constants, target and compiler
provenance. `mac-driver::export_driver!` retains that generated declaration in a
build-time ELF section. Packaging requires an exact match to the linked declaration,
then fills the actual code-envelope digest and emits DABI/EABI. The ELF section
is build evidence; ELF section tables/headers never enter a native resource fork.
LLVM's GNU retained-section OSABI marker is accepted by the build-time extractor.

The generic C entry receives a borrowed Host descriptor and initializes Instance.
Host exposes module identity, dependency resolution and broker invocation; it and
the code stay pinned until reboot. Instance carries an opaque u64 state and a
module-local executable dispatch address, validated before publication. Rust
traits, String/Vec, allocator ownership and private struct layouts never cross
this boundary. The SDK exposes borrowed slices to ordinary driver bodies.

Call carries a generation-tagged resident ticket, phase, class, class operation,
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

### Resident device service (VDEV, version 1)

Resident class providers use the same generated interface/request capabilities
as independently loaded modules. Their provider owner is the reserved kernel
owner zero. A module declares resident classes in `services`, not module
`dependencies`; resolution and every request check that declaration separately.
Only an admitted DRVR with a matching bus key can claim physical hardware.

`mac-device-interface` owns this class's byte layouts; `mac-driver::devices`
provides the Rust binding. These payloads do not alter the generic C entry ABI.
All fields are little-endian. Description is 48 bytes: claim handle u32 (zero
for probe), bus class four bytes, 16-byte match key, register base u64, register
length u64, interrupt u32, flags u32. Flags are coherent DMA=1, MMIO=2,
resident-owned=4. Mapping is 24 bytes: generation-tagged handle u32, zero u32,
device address u64, length u64. Invalid lengths, overflow and reserved bits fail.

| Op | Input | Output |
| --- | --- | --- |
| 0 Probe | bus ordinal u32 | 48-byte description, no claim |
| 1 Claim | bus ordinal u32 | 48-byte description with owned claim |
| 2 Map | claim u32, zero u32, module RAM address u64, length u64 | 24-byte mapping |
| 3 Reset | START: claim u32; POLL/CANCEL: empty | empty; Pending until stopped acknowledgement |
| 4 Unmap | mapping u32 | empty; refused while DMA can still run |
| 5 Release | claim u32 | empty; requires no mappings/pending reset and stopped device |

Claims and mappings use centrally registered shared handle namespaces plus
independent module-owner checks. A reset associates its broker-issued ticket
with the claim; polling and cancellation use that incarnation without retaining
input pointers. No operation spins waiting for hardware. RAM mappings are
restricted to the caller's pinned native module reservation. The current QEMU
virt bus uses coherent identity DMA; this is not an IOMMU/isolation claim.

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
The initial budget checks are admission limits, not an MMU or heap quota.

On the current 128 MiB QEMU virt board, the machine adapter reserves
`0x87800000..0x87e00000` for modules. This leaves the ROM's live storage scratch
heap at `0x87000000..0x87800000` and the high2 MiB device-tree reservation untouched.
These addresses belong to the machine adapter, not the class-neutral registry.

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

Portable host components are `f-` or `d-` followed by 32 lowercase hexadecimal
identity digits. These fixed 34-byte tokens avoid Windows reserved names and
UTF-8 expansion beyond host component limits. The exact guest name is recovered
from the container metadata; it is never truncated or inferred from the token.
The token's kind/identity must match the contained metadata. Root metadata lives
in `.system-risc-root`; each directory has `.system-risc-directory`. A root-only
`.system-risc-lock` serializes cooperating transactions. These records are hidden
from guest directory listings. Unwrapped files, misplaced internal records,
identity/token mismatches, duplicate catalog/object identities and canonical
name collisions are errors, not silently merged or ignored entries. Importing a
copy assigns fresh identities and catalog IDs; copying a raw container does not.

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

## Host directory wire protocol (RSHOST01, version 1)

`mac-host-protocol` owns the allocation-free codec and incremental reply decoder.
The VM byte channel carries a 48-byte little-endian header followed by exactly
`payload_length` bytes (maximum 65536). There are no native pointers, Rust enum
layouts or implicit host paths on the wire. Unsupported versions require a
matching rebuild; there is no older decoder.

| Offset | Bytes | Meaning |
| --- | --- | --- |
| 0 | 8 | `RSHOST01` |
| 8 | 2 | whole protocol version, 1 |
| 10 | 2 | header length, 48 |
| 12 | 4 | payload length, at most 65536 |
| 16 | 4 | nonzero correlation ticket, echoed unchanged in reply |
| 20 | 2 | operation below |
| 22 | 2 | 0 request, 1 reply; other bits invalid |
| 24 | 4 | session capability; zero only during Hello |
| 28 | 4 | object capability |
| 32 | 8 | operation-specific offset |
| 40 | 4 | operation-specific value |
| 44 | 4 | signed status; requests must contain zero |

The guest broker's generation-tagged request ticket supplies correlation; it
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

Unused request fields must be zero. Rename encodes `u16 first_name_length`, first
name, then second name. Enumerations stop with Stale if their directory changes.
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

### Host directory transport class (`HDIR`, version 1)

The loadable Host Transport driver publishes the serialized, exclusive `HDIR`
class. This is a byte/directory transport, not a block device and not a Macintosh
catalog implementation. The separate filesystem provider consumes it. Operations
use the `RSHOST01` operation numbers above. START input is a 24-byte little-endian
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
it. POLL/CANCEL have empty inputs. A completed output is the full checked 48-byte
`RSHOST01` reply header followed by its exact payload. Transport errors and host
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
