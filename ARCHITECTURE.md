# Resident System and Toolbox clients

Applications use the public `toolbox/*` crates for Rust interfaces. These crates
marshal requests through `mac-traps`; they contain no resident manager state,
rendering engine, font strikes or heap. `mac-toolbox` groups them behind a
loader-initialized `System` handle. `mac-abi` has no dependencies and defines
the versioned C layouts, protocol constants and shared value types.

The native dispatch path is:

```text
Finder / native application / definition resource
  -> public Toolbox binding
  -> mac-traps typed request
  -> loader-supplied gate -> RV64 ECALL
  -> System machine trap entry
  -> system::toolbox::<manager>
```

The RV64 gate uses `a7` for the operation, `a0` for the request pointer and `a1`
for its byte size. ABI 7 supplies one gate and capability bits in the 24-byte
`SystemServices` structure. [FORMAT.md](FORMAT.md) describes the exact contract.
Applications and System must be rebuilt together when this ABI changes.

## Unreleased compatibility policy

System RISC rebuilds ROM, System, applications, definitions, extensions, drivers
and tooling together. During this phase, internal API/ABI and native format/schema
changes may break previous project builds without a fallback or migration path.
Update all producers, consumers and tests in the same change, regenerate generated
images as needed, and remove the superseded implementation. Do not retain shims
or dual code paths solely for earlier System RISC artifacts. Version/schema and
bounds checks still reject incompatible input with a clear rebuild/recreate
diagnostic; a version field is not a promise to load older layouts.

Compatibility with original Macintosh applications, classic media and explicitly
supported external formats remains part of the product requirements.

## Native application execution

Finder and disk-loaded native applications run on separate OS-scheduled threads.
The one-hart RV64 scheduler uses CLINT timer preemption, independent 256 KiB
stacks and full integer-register frames, with a maximum of eight concurrent
application threads including Finder. Application progress does not depend on
`WaitNextEvent` or cooperative yield calls. Kernel traps execute with interrupts
masked; blocking queues, native worker-thread APIs and multi-hart execution are
separate future gates. See [THREADING.md](THREADING.md) for ownership and limits.

`toolbox-processes` exposes launch, enumeration, activation, hiding and exit through
`System::processes()`. Finder opens APPL catalog entries through that service.
[QuickDraw Test and TextEdit Exerciser](apps/README.md) are independent
applications with resource forks, installed under `Applications`; their code is
no longer part of Finder. Each app owns its resource-defined menus/windows and calls only the
aggregate Toolbox facade. Closing its last window or choosing Quit returns from
its native entry point; System cleans up its state and retires its thread stack.

The resident right-hand application menu uses live Process Manager state. It
switches foreground windows, ordinary menus and keyboard event ownership and
provides Hide, Hide Others and Show All. The menu belongs to System rather than
Finder. Background applications retain independently scheduled execution.
Original presentation and actual multi-app input behavior have their own
[launch-slice acceptance gate](TOOLBOX-PLAN.md#first-native-application-launch-slice).

The loader validates NABI descriptors, compact RV64 segment streams and RLOC
patches. ELF is confined to build-time extraction. System retains its fixed board
reservation; independently loaded definitions relocate into a disjoint module
pool, and Finder owns
`0x81000000..0x82000000`. Native application manifests specify preferred ranges.
The image registry reserves each application's full declared page-aligned span and
can relocate new `RLOC` packages into a free span in `0x83000000..0x87000000`.
All pointer patches validate before destination memory is written. Different
files get separate code/data/BSS, including copies of the same executable.
An eight-slot immutable image cache includes Finder; relaunch of a stopped file
zeros its reservation and restores its segments. Launch of a running volume/CNID activates its existing
process. This does not provide MMU isolation, hot replacement, general dynamic
linking or original68k translation. Native code remains trusted shared M-mode.

## Resident implementation

`system/src/toolbox` contains QuickDraw, windows, controls, events, resources,
files, menus, dialogs, memory, scrap, textedit and processes modules. The
implemented managers own the rasterizer, bitmap/TrueType font manager, window state, retained drawing lists,
offscreen worlds, cursors, icon and PICT decoders, event queue and shared read-only
resource search, HFS catalog/Trash mutations and bounded handle Memory Manager.
The resident Menu Manager tracks resource-defined menus and invokes MDEF0 for
sizing, drawing and hits. The initial Dialog Manager interprets ALRT/DITL and
calls WDEF1/CDEF0, with modal input routing. The shared Scrap Manager owns bounded
binary-flavor records and native UTF8 text behind a separate nonwaiting guard.
The Event Manager separates its128-entry native application queue from the
captured20-entry low-level OS queue; physical input state is observed before
window/menu/dialog routing. Monochrome TextEdit owns generation-checked editor
records, fallible UTF8 editing/layout and clipped retained drawing. Native
clipboard edits stage changes while holding the desktop then Scrap guards; no
borrowed text escapes. Full classic managers remain incomplete.

The Window Manager controls lifecycle, z-order, exposure and interaction. Each
loaded WDEF supplies its own frame drawing, hit testing and geometry. The
standard WDEF and button CDEF are independently linked resources, with all their
implementation in their respective `src/main.rs`. Their only drawing dependency
is the public QuickDraw client: OS primitives and fonts remain resident. There
is no standard-WDEF implementation trap in System. Host tests and the reference
renderer compile the resource's same source file for verification.

Most manager logic forbids unsafe code. Unsafe operations are concentrated in
loader/entry adapters, hardware access, raw trap payload handling, resource-entry
invocation and transport. The runtime borrows desktop state under a reentrancy
guard; stateful recursive calls fail. Drawing/font traps can nest during a WDEF
call without reborrowing the runtime. System/font storage is published once, and
each cached application resource image is immutable. Resource queries select the
trusted caller or retained-window owner. Readonly resource queries also bypass the mutable desktop borrow, including
queries nested inside a definition. Resource data moved into the font manager
remains visible through the System resource scope. The current trusted M-mode
environment has shared memory; these checks are ABI validation, not process isolation.

## Planned control panels and kernel extensions

[Platform workstream P6](docs/CORE-TOOLBOX-ROADMAP.md#p6--control-panels-and-kernel-extensions)
plans kernel-resident Rust extension modules with a small `no_std` SDK, typed
service/hook registration and relocatable resource packaging. Resource-based
control panels remain ordinary Toolbox applications on their own OS threads,
configuring resident services through versioned JSON preferences. See the
[design and delivery gates](docs/SYSTEM-EXTENSIONS.md) for boot phases,
dependencies, registration ownership, disabled-startup recovery, fatal kernel
panic behavior and the separate legacy cdev/INIT adapter track. None of this
implies a loader, hot unloading or fault isolation exists today.

## Planned networking stack

[Platform workstream P7](docs/NETWORKING.md) plans resident Rust networking with
shared protocol/socket state, thin Toolbox bindings and a future `std::net`
platform adapter. Machine drivers own DMA/interrupts; preemptible network work
and monotonic deadlines drive protocol progress independently of UI input.
The plan covers dual-stack TCP/IP, configuration/DNS/discovery, JSON-backed
control panels and separate legacy networking adapters. Blocking I/O, entropy,
TLS and concurrent execution each retain their own delivery gates.

## Planned Rust standard library

[Platform workstream P8](docs/RUST-STD.md) maps std's platform backends onto
resident allocation, runtime, thread/TLS/wait, stream/file/process, clock and
network services. A patched target/sysroot calls the minimal core-only native
ABI/transport, independently of the application Toolbox facade. Resource-based
apps may use std while System, definitions and kernel extensions stay no_std.
The plan stages runtime, concurrency, document/Finder and network acceptance;
neither a custom target nor a linked unsupported backend establishes std support.

## Planned terminal, hosted tools, isolation and drivers

[P11](docs/TERMINAL.md) adds a Terminal APPL and terminal-only CLAP process class,
with checked session ownership and JSON preference-defined base environments.
[P12](docs/HOSTED-TOOLCHAIN.md) stages hosted Rust tools after the actual std,
storage, child-process and linker prerequisites.

[P13](docs/PROCESS-ISOLATION.md) replaces the current shared M-mode app execution
with protected user processes and checked syscall/callback boundaries. This is
planned hardware protection, separate from today's owner/generation checks.
[P14](docs/DRIVERS.md) keeps block storage built in and moves other drivers into
trusted in-kernel extension modules. Driver packages require DRVR classification
and mandatory DABI build-contract metadata checked before code entry; resource
inspection alone never loads them. Kernel modules are not isolated user apps.
Native drivers are installed in the blessed `System Folder/Drivers` directory;
System extensions are installed in `System Folder/Extensions`. Both paths are
relative to the selected blessed installation. Their shared module loader keeps
discovery and entry-contract validation distinct; folder placement alone never
authorizes execution.

## Native HFS-B storage

[P15](docs/CORE-TOOLBOX-ROADMAP.md#p15--hfs-b-and-early-system-migration) implements
bootable [HFS-B](docs/HFS-B.md) for normal System/workspace images. ROM requires
an HFS-B installation and System mounts that same selected disk/partition.
`mac-filesystem` dispatches HFS-B and unchanged classic HFS through common block
traits and resident File/Resource Manager services. Classic HFS remains available
for reference media and file exchange; live in-place conversion is out of scope.

The safe no_std `mac-hfs-b` crate owns codecs, formatting, validation, catalog,
allocation, both forks and all native filesystem algorithms. Linux tooling,
image builds, ROM and System share it. Version1 pins Unicode17.0.0, NFC storage,
canonical caseless comparison, 255-byte names and 1904-epoch microsecond dates.
The Linux adapter supplies sector-file I/O, locking and streaming host copy;
platform I/O and UI do not belong in the shared crate. The future System RISC
Filesystem utility will use public Toolbox services and the same core.

Fork lengths and read offsets stay64-bit through the public File Manager. The
shared core and Linux tool stream reads/writes without whole-fork allocation.
The current atomic whole-data-file SDK writer still stages at most4MiB; larger
streamed/random userspace writes need a separate service rather than raising its
buffer limit. Resource startup reads validate a bounded map and allocate selected
payloads only, with an explicit4MiB aggregate selected-payload budget on the
current8MiB heap. This is not a total-fork-size limit or an unbounded allocation:
ROM selects executable metadata/code, and inspectors read individual ranges.
A bounded64-sector cache avoids rereading unchanged metadata, updates after
successful writes and clears on failed reads/writes/flushes so recovery sees the
actual media. File-backed resource-fork editing remains future Resource Manager work.

Documents and versioned JSON Preferences use the lossless offline copy-in/copy-out
workflow in the [host tool](tools/hfs-b/README.md). Preserve user bundles outside
generated-media cleanup; do not retain obsolete HD images for that purpose.

## Planned archive and host-directory volumes

[P16](docs/ARCHIVE-VOLUMES.md) mounts StuffIt files as read-only filesystem
volumes with on-demand decoding and both forks intact. Finder requests a mount;
the resident backend owns parsing, source leases and lifetime. Applications use
ordinary File/Resource Manager services, including copying out to writable media.

[P17](docs/HOST-FOLDERS.md) adds an independent vhost filesystem/block-device
driver pair. Its device side owns host transport; its filesystem side maps a host
directory into ordinary guest files. One versioned host container per file holds
both forks and metadata, transparent to apps. Directory operations have an
explicit contract separate from actual block exports. Both drivers require
DRVR/DABI validation. Shared Linux tools reuse the container codec. Host sharing
and archive mounts follow P15; neither is required to boot the native System.

The agreed implementation sequence is built-in HFS-B and shared tooling, then
the driver/module interface proven by StuffIt and vhost mounts, then the
[P18 host clipboard extension](docs/HOST-CLIPBOARD.md). The extension detects a
compatible virtual-host bridge and synchronizes the resident Scrap service;
apps retain their ordinary Toolbox APIs. Without the bridge, the guest clipboard
continues locally. Typed change subscriptions, revision/origin tracking and
bounded asynchronous transfers prevent echo loops and per-input work. This
sequence precedes broader driver extraction and userspace std migration.

## Classic applications

Original A-line trap identities remain separate from the native request numbers.
The initial classic adapter handles 32-bit registers, checked guest memory,
big-endian structures and original Pascal stack arguments for rectangle geometry,
point/rectangle mapping, basic text/font queries and drawing, monochrome cursors,
GetNextEvent, EventAvail and TickCount. Both adapters reach resident manager
logic. Unsupported traps explicitly return `NOT_SUPPORTED`.

This does not yet execute an original 68k application. The translator/execution
engine, WindowRecord-dependent adapters and remaining Toolbox services are
future work. The executable-resource registry already routes original code
resources to a translation backend when one is supplied; see
[CODE-RESOURCES.md](CODE-RESOURCES.md).

## Verification

Canonical compatibility tests live in `crates/mac-toolbox/tests` and use the
aggregate public SDK. A host-only `test-support` feature supplies scoped service
tables that call real resident dispatchers; it is a dev dependency, never linked
into an application. The no-std conformance functions are also included by the
RV64 launch harness, reaching the actual ECALL entry. Private-engine unit tests
remain auxiliary while older rendering comparisons migrate to the aggregate
suite. See [TOOLBOX-PLAN.md](TOOLBOX-PLAN.md) for the migration and evidence gates.

`cargo xtask check` tests host behavior, actual native/classic ECALL marshalling,
ROM boot and rejection cases, graphical input and resource dispatch, and WDEF
reference pixels. Original QuickDraw primitive, icon and bitmap-font comparisons
are strict; the report records remaining TrueType/style pixel differences while
requiring their measured widths and metrics to match. Packaged-resource checks
validate every imported payload and every FOND association target.
`scripts/check_system7_boundaries.py` checks normal dependency
closures and linked application/resource images for resident manager symbols
and embedded font strikes. Its result is included in `toolchain-checks.json`.

## Filesystem and Finder ownership

BootInfo is88bytes with firmware disk-info/read/write/flush callbacks
and startup volume identity. DiskInfo carries capacity, read-only state and the
driver icon resource identity. System serializes firmware access through its I/O
owner guard and dedicated synchronous execution stack. The I/O service context
is not independently runnable; its interrupt guard serializes current one-hart
access without declaring opaque firmware state Send. The File Manager owns
mounted-volume IDs, catalog traversal, application-owned independent fork marks
and mutation policy. Native dispatch captures AppId before the handoff; foreign
read/seek/tell/close calls fail, and exit closes only the app's marks. Classic HFS retains its bounded catalog/extent engine. Native HFS-B uses the same
shared read/write crate as the Linux formatter and ROM, including tree and fork growth. New Folder,
Trash, Put Away and recursive Empty Trash persist to the actual device; failed
writes/flushes trigger rollback and failed rollback poisons further mutations.
This is not a crash-atomic journal. Bounded native ordinary data-fork saves now
stage bytes in an owned generation-tagged writer and publish a copy-on-write
fork after its payload flush. Replacement preserves CNID and resource fork;
reported failures retain the old live file. HFS-B supports both forks and growing catalogs internally; public SDK resource-fork
editing remains future work. System owns StandardGetFile/StandardPutFile dialogs,
including filtering, destination navigation and replacement confirmation; the
Rust interface lives in toolbox/standard-file. See docs/RESEDIT.md for limits. Neither SDK nor Finder links
either filesystem engine. Finder aggregates actual per-volume Trash directories in a
virtual window and keeps selection identities as (volume,CNID) pairs.

The desktop is a retained application scene below windows and the menu bar.
Finder begins a staged desktop update, draws through ordinary QuickDraw, then
commits it atomically. Desktop activation deactivates a folder without changing
window order. CDEF1 owns scrollbar geometry, hit tests, drawing and thumb-value
mapping in its own executable resource; the Control Manager only retains and
dispatches it. The resident scroll-box manager composes CDEF1 with a virtualized
row viewport. It owns generation-tagged IDs, parent/owner validation, immutable
raster snapshots, pixel shifting and repeat timing. The thin controls SDK
supplies exposed rows and pumps tracking events. Viewport double buffers and
compositor caches share a bounded budget. See `docs/SCROLL_BOX.md` and
`analysis/desktop-compositor.md` at the repository root.

Stable window content commits accumulate exact local damage, intersect it with
the Window Manager's visible fragments, and replay only through those clips.
Scroll boxes damage their viewport and CDEF scrollbar, including previous bounds
when moved or disposed. Standard controls damage their own bounds. EndUpdate
publishes the window content area without invalidating other windows or chrome.
Definition-owned grow adornments remain above content and respect both damage
and application clipping. Structural scene changes still use the full compositor;
they must not be requested for ordinary content updates or cursor activity.

The compositor's backing scene contains no cursor pixels. A 16x16 save-under
temporarily holds the pixels covered by the cursor while presenting its overlay,
then restores the backing scene. Motion, shape/visibility changes and animation
publish only old/new cursor rectangles without scene replay. Scene damage under
a cursor updates its backing pixels before compositing the current cursor.
The opt-in event trace records the actual framebuffer-copy rectangles, enabling
public SDK tests to reject cursor-triggered scene replays and scroll damage
outside the viewport/bar rather than relying only on unchanged screenshots.

Scene revision advances on composition, independently of physical input and
cursor presentation. Ordinary input does not write UART state messages. Test
transports read a System-owned atomic UI observer through QMP physical-memory
inspection; its separate input sequence acknowledges consumption, and its
sequence lock rejects snapshots overlapping publication without pausing the
guest. This adds no application trap. An unchanged foreground menu bar is a
cached layer keyed by application and menu selection; menu/resource changes and
application-menu refresh invalidate it. Its fixed 53 KiB reservation is deducted
from the shared compositor budget. Popups remain separate definition-owned
overlays. Observer counters expose scene compositions and menu-bar rasterizations
so SDK trace acceptance checks that cursor motion changes neither and that
window tracking reuses the unchanged bar.

After half a second without a foreground event-loop poll, System temporarily
overlays the original System `acur` -6078 animated watch at six ticks per frame.
Applications blocked in event waits are exempt; background work does not affect
the foreground. Native event retrieval/peek and classic OS-event retrieval/peek
refresh progress. Clock, drawing and other service traffic do not mask a stalled
event loop. The override never edits application cursor state or visibility depth;
polling again or switching foreground restores the app-owned policy. The timer
presents only cursor damage, even when all application threads run without
yielding. Long interrupt-masked kernel services cannot be observed until they
return; this policy does not turn those services into preemptible operations.

Standard File lists resolve file-owned custom icon families (-16455 with the
Finder custom-icon flag), then bundle/FREF or resident creator/type mappings.
An unavailable file icon falls back to the standard document; directory rows
retain their folder icon. Only visible rows are cached, keyed by catalog
identity and metadata, with cache eviction on navigation/list refresh. Immutable
icon families are shared with retained drawing commands, so selection and caret
redraws neither reread resource forks nor decode/copy the icon pixels. The dialog
content renderer still rebuilds its content on selection changes; this cache is
not a claim of incremental row painting.

## Application-local namespaces

Window IDs are checked against the caller's owner in the shared registry. Other
manager IDs, including Memory, TextEdit and offscreen worlds, are meaningful only
inside the trusted application's selected manager namespace. Equal numeric IDs
in different app namespaces do not authorize access to one another's objects.
Memory cleanup retains slot generation counters when a scheduler slot is reused.
Per-app menus, events, dialogs, worlds, editor records and legacy compatibility
state are disposed at exit; the system clipboard and immutable cached images
remain system-owned. These are service-level ownership contracts in trusted
shared memory, not MMU-enforced isolation. Additional native threads sharing an
AppId, original cooperative contexts and original low-memory virtualization still
require their own context and synchronization contracts.
