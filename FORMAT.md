# Native resource executable format, version 1

The container is a classic Macintosh file. Its resource fork uses the standard
big-endian resource header, data area, map, type/reference lists and Pascal names.
The data fork is currently empty. Host transport uses MacBinary II with a valid
CRC-16/XMODEM header and 128-byte fork padding. File/resource names use Mac Roman.

The complete executable ELF is resource `'RV64'` ID 0. A companion `'NABI'`
resource ID 0 has exactly 32 bytes, all numeric fields big endian:

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | Magic `RVN1` |
| 4 | 2 | Container format version, currently 1 |
| 6 | 2 | Role: 1 = resident System, 2 = application, 3 = code resource |
| 8 | 4 | Native System service ABI version, currently 5 |
| 12 | 4 | Reserved, must be zero |
| 16 | 8 | Inclusive load reservation base |
| 24 | 8 | Exclusive load reservation limit |

The descriptor's version and ABI must be supported. The load reservation must
fit available guest RAM and must not overlap the bootstrap, other resident
images, stacks or heaps. The build verifies non-overlapping System/Finder
reservations; an actual loader must check all resident allocations.

The ELF must be ELF64, little endian, System V ABI, `ET_EXEC`, `EM_RISCV`,
version 1, with 64-byte ELF and 56-byte program headers. The initial contract
allows only the RVC flag and the soft-float calling convention. Current Rust
builds target RV64IMAC; the ELF header alone cannot verify every instruction
extension. Dynamic linking, an interpreter and TLS are rejected. Native modules
are linked with relaxation disabled to avoid dependence on per-image `gp` state.

For each nonempty `PT_LOAD`, the loader must:

1. Validate file offsets, sizes, alignment, flags and the complete memory range.
   Physical and virtual addresses are equal in this initial identity-mapped
   environment. Segments must not overlap and must fit the image reservation.
2. Copy `p_filesz` bytes from `p_offset` to `p_vaddr`.
3. Zero the remaining `p_memsz - p_filesz` bytes, including BSS.
4. Synchronize instruction fetch before running newly copied code (`fence.i`
   on the target machine), and enforce segment permissions when memory
   protection is available. Writable/executable segments are rejected.

The ELF entry must be even and within file-backed executable segment data.
Before calling it, supply a valid stack aligned to 16 bytes and establish the
chosen privilege/trap environment. Module entry points are C functions, with
their first argument in `a0` and return value in `a0`, called with a valid `ra`.
They do not initialize CPU state, a stack, hardware or their own BSS.

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
ABI 5's `SystemServices` is 24 bytes on RV64:

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 8 | ABI version 5 and structure size |
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
caller. All code currently runs in M-mode with shared memory, without an
application scheduler or memory isolation. Payload pointers must obey their
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

The separate QEMU ABI smoke harness tests this call sequence with executable payloads
extracted from the generated resource forks. QEMU's ELF loader performs the
memory loading for that test. The [boot ROM](bootrom/README.md) implements and
tests the actual virtio/HFS/resource/ELF loading path separately. System performs
Finder and definition-resource loading through its startup callback. The normal
disk-boot checks wait for `DESKTOP_READY`, after Finder's first update events.

Runtime ELF resources omit symbols and debug sections; full linked debug ELFs
remain in the build output. Native executable definitions use a role-3 descriptor
followed directly by ELF bytes inside their original resource type/ID, rather
than a nested resource fork. See [CODE-RESOURCES.md](CODE-RESOURCES.md).

These interfaces are an evolving native prototype. The structure-size checks
reject older tables that lack required fields even while the prototype ABI
version is 5. Rebuild ROM, System, Finder and definitions together.

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
use mac-abi/src/controls.rs and dispatch CDEF1; their inline state does not expose
classic ControlHandle or WindowRecord ABI.

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
