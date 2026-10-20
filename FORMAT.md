# Native resource executable format, version 1

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
startup-volume context/read callback. The latter reads a named file's resource
fork from the same blessed folder selected during boot; null output queries its
length. It remains valid while System/Finder execute. System initializes its
own heap, graphics, input and window services, loads Finder and calls its entry.
This call normally remains in Finder's event loop. Without the startup callback,
System returns a non-graphical service table for the isolated ABI smoke test.
Incompatible arguments return null. The System image stays resident.

Application entry:

```text
unsafe extern "C" fn(*const SystemServices) -> i32
```

An application validates the table before calling the loader-supplied trap gate.
ABI 6's `SystemServices` is 24 bytes on RV64:

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
separately tests real virtio/HFS/resource/segment loading; System loads Finder and
definition resources through its startup callback. Normal disk-boot checks wait
for `DESKTOP_READY`. Rebuild the complete unreleased system on format changes.
See [CODE-RESOURCES.md](CODE-RESOURCES.md) for executable resource families.

ABI5 BootInfo adds optional write_sector and flush_disk callbacks to the storage
contract introduced by ABI4. Its size is104bytes on RV64; SystemServices remains
24bytes. Device ordinals stay stable while System runs; NOT_FOUND ends device
enumeration. Each read or write transfers exactly512bytes; flush is an explicit
durability barrier. Callback contexts and DMA queues remain firmware-owned and
are serialized by the resident owner guard. DiskInfo is24bytes and carries sector
count, read-only state and the driver icon resource identity. Missing write/flush
callbacks make a volume read-only. Rebuild all native modules together for ABI5.
Window and retained drawing titles accept768 UTF8 bytes, enough for all255
MacRoman characters after conversion. All inline font/resource/control title
buffers use the same capacity. File/volume name buffers remain sized for HFS31.

FILE requests and metadata are defined in mac-abi/src/files.rs. File IDs are
opaque mounted-volume IDs plus persistent HFS catalog IDs; open handles include
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
the chosen dates, wall-clock packaging time and optional source epoch. HFS image
creation preserves file dates on import, creates the volume and initial folders
at the real build time, and records its UTC creation interval for SDK validation.
Finder Get Info displays the catalog timestamps rather than a synthetic date.

Native Rust `Timestamp` holds signed64-bit Unix seconds. HFS/MacBinary dates are
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
Ctrl key equivalents use the same event path. This is a native extension, not a
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

CLOCK0x10018 returns signed64-bit Unix wall-clock seconds. The QEMU virt adapter
serializes Goldfish TIME_LOW/TIME_HIGH reads; no HFS epoch is imposed on native
Timestamp. HFS writes explicitly check conversion to unsigned1904 seconds.
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
