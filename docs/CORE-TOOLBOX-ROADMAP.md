# Core Toolbox remaining work

Assessment baseline: `320eee1` (native application-launch milestone).

17 deliberately grouped workstreams, not an API count or a completion percentage. Status describes the current tested native slice; effort is a qualitative planning judgment about the remaining contracts.

All 17 workstreams have remaining work. Ten have usable tested native slices, four have narrower foundations, and three groups have no public manager. These are capability groups, not equally weighted API counts. None is signed off for full original System 7 parity.

Core date/time, fixed/fract arithmetic, queue and truthful Gestalt/availability utilities are cross-cutting R1 work rather than separately counted manager groups. General Rust std support remains a separate platform effort alongside these services.

P15's native HFS-B boot and default System-volume cutover passed the full
toolchain gate on 2026-10-20. Shared read/write tooling, resident storage, ROM
startup and bounded/targeted resource reads now supply that foundation. The
selected-payload and whole-file SDK write budgets remain explicit; this is not
unlimited resident allocation or complete File/Resource Manager functionality.

After that cutover, establish the driver interface with StuffIt and vhost mounts,
then deliver the host clipboard extension, in the sequence below. Native
document-app/Toolbox work and std migration follow those initial validation gates.
Sound, printing, networking and full non-Roman scripts remain separate expansion
tracks, outside this core count.

| Workstream | Current native surface | Remaining effort | Important gaps |
| --- | --- | --- | --- |
| Memory | Usable tested slice | L | Pointer/master-pointer ABI, heap zones, pressure-driven purge/compact and grow/purge callbacks. |
| Resource Manager | Narrow foundation | L | Open/current/close resource files, mutable chains, resource handles and detach/purge; add/write/update after fork writes. |
| QuickDraw / PICT / icons | Usable tested slice | L | Complete port/recording lifetimes, grafProcs, advanced transfers/patterns, indexed worlds and remaining PICT semantics. |
| Color / Palette / devices | Narrow foundation | XL | Actual Palette Manager, GDevice/depth lifecycle, color matching, indexed conversions and palette animation. |
| Fonts / Roman text | Usable tested slice | L | Measured TrueType/style pixel differences, fractional spacing/kerning, text policy, Roman word/collation/formatting services; emoji fallback (P4), Fonts folder (P9), OpenType/color (P10). |
| Events / clocks | Usable tested slice | M | WaitNextEvent sleep/mouse regions, scheduler wakeups, timer/caret/double-click policy, suspend/resume and extended pointer input (P3). |
| Windows / WDEF | Usable tested slice | M | Full query/visibility/hilite/behind/refcon lifecycle, custom definition callbacks, regions and classic records. |
| Controls / CDEF | Managed standard controls | L | Buttons/checks/radios/scrollbars have owned IDs and resident tracking; full control lists/properties, popup/user controls, action callbacks and keyboard focus remain. |
| Menus / MDEF | Usable tested slice | M | Dynamic editing, hierarchy/popups/scrolling, complete icon/style variants, custom lifecycle, flashing and System-owned Apple menu (P5). |
| List Manager / LDEF | No public manager | L | Cells, owned data, selection, scrolling, clicks/double-clicks, drawing and LDEF callbacks. |
| TextEdit | Usable tested slice | L | Editable-dialog integration, styled text, other fonts/alignment, word selection/autoscroll, TEHandle/private scrap ABI. |
| Scrap | Usable tested slice | M | Disk Scrap.dat, original counted-MacRoman/private TextEdit scrap and process suspend/resume integration. |
| Dialogs / alerts | Narrow foundation | L | General DLOG, editable/user items, tab focus, item mutation, modeless/filter callback contracts and alert stages. |
| Files / HFS persistence | Native HFS-B read/write slice | L | General streaming public fork-write/resize APIs, full metadata operations, eject/unmount, exclusive device access and the graphical Filesystem utility. Shared HFS-B allocation/tree growth, rollback, boot and image cutover are implemented; classic HFS exchange remains available. |
| Standard File / aliases | No public manager | L | Open/save dialogs and filters/hooks, alias records/resolution across moves and ownership of replies/resources. |
| Processes / application life | Usable tested slice | L | Blocked/wakeup states, document/quit parameters, native workers, relocation, legacy cooperative Thread Manager (P2), segments, CLAP sessions (P11) and process isolation (P13). |
| Apple events / core IPC | No public manager | L | Descriptors/coercion, handlers, open-document/print/quit events, replies/timeouts and process routing. |

M means a bounded extension of a working manager; L means substantial lifecycle/behavior work or a new manager; XL means a broad storage/device foundation. These are relative planning judgments, not engineer-weeks or measured completion fractions. Parallel work and new original-system findings can change the order.

## Delivery sequence

The [public Rust Toolbox API audit](TOOLBOX-RUST-API.md) is verified at ABI 7.
**Next: class-neutral driver/module SDK foundations**, exercised by StuffIt and vhost mounts,
then the host clipboard extension. P15's native boot/image cutover is verified
using the existing no_std applications. The R1–R6 sequence below describes
subsequent Toolbox delivery; its numbering does not put it ahead of the agreed
platform sequence.

The agreed implementation order is:

1. **P15 (native cutover verified):** shared HFS-B core, Linux tooling, built-in
   kernel/ROM support and native System-volume cutover. Keep bootstrap storage
   self-contained and its acceptance gates passing.
2. **P14 plus necessary P6 foundations:** define the driver/module SDK, class
   contracts, DRVR/DABI validation, loading and owned registration. Prove that
   interface with **P16 StuffIt archive mounts** and **P17 external vhost mounts**,
   including the vhost filesystem/block-device driver pair. These are working
   loadable test implementations, not just mock providers or built-in shortcuts.
3. **P18 plus its P6 service hooks:** a host clipboard extension detects a
   compatible virtual-host bridge and shares the resident Scrap service with the
   host. Prove extension lifecycle and change notification beyond storage.

Take only the scheduler, resource-loader and typed-service prerequisites needed
for each gate; the full P6 control-panel suite, SMP, networking and std port are
not prerequisites. Resume broader driver extraction, std and Toolbox delivery
after this sequence. Each milestone remains subject to its runtime acceptance.

2026-10-21 checkpoint: the actual FSYS/HDIR release modules relocate and reach
the Linux companion in a private guest. Public Toolbox host-export count,
mount, duplicate-mount reuse, volume metadata and unmount now run through owned
resident jobs with native thread park/replay. Guest assertions cover writable
and read-only sources, invalid-source cleanup and stale/foreign volume IDs after
slot reuse. Catalog, direct-child enumeration and parent queries also pass native
SDK checks for nested folders, Unicode names and read-only roots. Read leases
prevent unmount/reuse during pending catalog I/O; mounting no longer enumerates
the root. Both-fork open/read/seek/tell/close, independent marks, EOF, Drop and
stale fork IDs pass through the public SDK in the guest. Deferred resident exit
cleanup retains marks until Close acknowledgement; a real second-app guest test
forgets two forks, exits and successfully unmounts after cleanup. Public path
lookup/output pass Unicode, relative/dot/parent, root and bounded-output checks;
metadata-only ancestry does not enumerate unused directory children.
Public incremental cursor scans now pass in the guest, including remount binding;
unknown child counts/allocation use Rust options, with Finder preserving unknown
folder allocation. Resident tests cover cursor revision/field/progress checks.
Native bounded data saves now pass creation, replacement, resource-fork
preservation, exact-ID/reader contention, cursor invalidation and write-token
reuse checks. The Pascal-string trap path shares those resident services. A real
second app also exits with a forgotten staged writer without publishing it;
uncertain remote-write ownership/exit retention has resident tests. Visible
volume/folder dates change after publication, including retryable cleanup.
Resource-file inspection now passes against a real host-mounted ResEdit through
the public SDK: all types, metadata, names and bytes match the startup-disk copy,
including payloads spanning class-read batches. Cached queries avoid provider
jobs, positioned inspection preserves fork position, and stale resource-fork
marks fail after slot reuse. Original Pascal resource calls share the service.
This is partial P17: application-loading dispatch, remaining
folder/metadata/rename/move/delete and full two-fork copy operations, refresh/watches,
normal launcher exports, disconnect/pending-I/O-exit/performance acceptance and P16 remain
to be completed. See [the exact filesystem contract and coverage](FILESYSTEM-INTERFACE.md).

Finder's [selection performance foundation](FINDER-SELECTION-PERFORMANCE.md)
now caches folder/desktop metadata and wrapped labels, applies selection diffs
through exact masks and early-outs on unchanged selection. Native guest gates
cover multiple selection, marquee cancellation, bounded overlays and filesystem
revision refresh. Persistent icon placement is attached to catalog identities;
same-volume File Manager moves preserve it along with dates, comments and both
forks. The remaining [Finder selection and Drag Manager work](FINDER-SELECTION-DRAG.md)
is resident drag tracking/negotiation and cancellable batch copy/move with
Option/Command sampled at drop time, managed progress controls and provider
metadata/move support. Trash always moves. Keep these remaining items distinct
from the completed cache and selection substrate.

### R1 — Service lifetimes and waits

Depends on: the current ABI7 baseline.

An idle app sleeps and wakes for input/timers; resource files/chains and handles have owned lifetimes; basic truthful utility queries exist.

### R2 — Complete the everyday UI objects

Depends on: R1.

A resource-defined settings dialog edits text and check/radio/list values, tabs focus, validates and cancels correctly; window/menu/control/list lifetimes and callbacks are covered.

### R3 — Open, save and persist documents

Depends on: R1, R2.

TextEdit opens/edits/saves a real document through Standard File; data/resource forks and JSON preferences survive reboot; resource writes, alias moves and failed writes are verified.

### R4 — Graphics and text fidelity

Depends on: R1.

Close measured font differences; add styled text/Roman utilities, indexed colors/palettes, remaining PICT/port/callback contracts and accepted animations against the original.

### R5 — Application document workflows

Depends on: R1, R3.

Finder opens a document in the right app, delivers core Apple events and handles replies/quit/save/cancel; blocked/wakeup and cross-app clipboard behavior are verified.

### R6 — Classic compatibility closure

Depends on: R1, R2, R3, R4, R5.

A deliberately selected original 68k test app runs translated with real records, pointer/handle and Pascal A-line ABI, callbacks, globals and segment/resource lifetimes. General app compatibility remains app-by-app evidence.

R2 and R4 can progress independently once their relevant R1 contracts are ready. R3 can begin its filesystem engine alongside R2, but its end-to-end open/save exit needs the dialog/control services. R6 ABI work can begin earlier against selected APIs; its integrated application acceptance depends on the earlier slices and a working translator.

## Planned platform work

Platform numbers identify workstreams, not execution order. P15's HFS-B system/
workspace cutover is the very next implementation priority, ahead of all other
expansion tracks except its own prerequisites. Bootable HFS-B is required for the
target: after its boot/write-integrity gate, new normal images must put
System/Finder on HFS-B. Keep
classic HFS mountable for compatibility; HFS boot is transitional, not the final
default build.
System growth is already a near-term concern: remove fixed small resource-loader
ceilings as an early storage gate, independently of the chosen boot filesystem.

### P1 — Multi-hart execution (SMP)

Status: required memory-model scheduler capstone; additional harts currently
park in the boot ROM. Deliver this as its own isolated extension milestone after
the model's memory/isolation gates.

The **Multi-Scheduler Extension** supplies replacement scheduling policy and chooses
when to activate secondary harts. Keep the resident single-hart boot scheduler
and minimal architecture mechanisms below a checked takeover seam. A rejected
activation must leave boot scheduling dependable. The extension is the memory
model's isolated scheduler capstone after its memory and isolation gates.

- Give each hart its own boot/trap stacks, current-thread context, resource
  selection, timer setup and idle state. Remove single-hart assumptions from
  scheduler and synchronous I/O context selection.
- Audit shared manager, allocator, resource-publication and device access.
  Define lock ordering, blocking/wakeup rules and explicit compositor/input/I/O
  ownership; masking local interrupts cannot provide cross-hart exclusion.
- Add secondary-hart startup, concurrent scheduling and inter-hart wakeups.
  Establish pinned-thread execution first, then separately verify migration.
  Legacy cooperative contexts remain inside their application's OS threads.
- Acceptance: public SDK guests on two and four harts demonstrate simultaneous
  application progress without Toolbox calls or yields, correct ownership and
  caller context, wakeups without lost events, and safe allocation/file access
  and exit under contention. A thread must never execute on two harts at once.
  Keep the one-hart regression profile, Cursor Test responsiveness, event latency
  and bounded present/repaint timings as required gates.

See [THREADING.md](../THREADING.md#delivery-gates) for the synchronization and
execution contracts. A successful multi-hart boot with parked secondary harts
does not satisfy this milestone.

### P2 — Legacy Thread Manager (cooperative threads)

Status: backlog; required for legacy application compatibility. This can progress
alongside P1, using the application's existing preemptive OS thread as its host.
It depends on legacy context/callback switching and blocking/wakeup foundations;
translated 68k execution is a separate prerequisite for the original-app gate.

- Inventory and capture the installed System 7.6.1 Thread Manager version,
  Gestalt availability, `_ThreadDispatch` selectors and error behavior.
  [Apple's Thread Manager manual](https://leopard-adc.pepas.com/documentation/macos8/pdf/ThreadManager.pdf)
  guides the inventory; the pinned original runtime resolves version differences.
- Cover `NewThread`, `DisposeThread`, directed/any-thread yields, thread state,
  pools and stack queries, critical sections, custom scheduling, switch and
  termination callbacks. Advertise only implemented styles and selectors.
- Preserve original register/FPU/stack context and A5/global-data sharing,
  including documented custom switch behavior. Capture which Toolbox/low-memory
  fields are shared or saved; do not make all application globals thread private.
- Preserve original reserved ThreadID aliases. Other context IDs use the shared
  generation-tagged facility with separate application ownership validation.
- Only one cooperative context within an application executes at a time, even
  on a multi-hart System. Legacy critical sections govern that application's
  Thread Manager scheduling; they never disable kernel preemption of the app.
  A legacy context that never yields must not prevent other applications running.
- Acceptance: original-system probes followed by canonical public SDK tests for
  scheduling order, explicit yields, stop/wake, context and shared globals,
  callbacks, disposal, stale/foreign IDs and exit cleanup. Add a translated 68k
  probe using the original traps and repeat coexistence tests on one, two and
  four harts while another application makes independent progress.

### P3 — Extended pointer input

Status: backlog. Add secondary mouse buttons and horizontal/vertical wheel input
through a versioned native Toolbox event interface. Preserve classic event
records, masks, primary-button state and ordering; unmodified legacy apps do not
receive unfamiliar events or synthetic clicks/keys. Optional compatibility
behavior needs explicit, verified support.

Resident scrollable UI handles wheel actions through shared services; native
applications can receive extended input and provide resource-based context menus.
Specify targeting, modality/capture, precision, single-consumer routing and
bounded coalescing. Include Finder, ResEdit, TextEdit and Standard File acceptance,
classic event/polling regressions, and cursor/repaint/dispatch performance gates.
See [the extended pointer contract](../EVENTS.md#planned-extended-pointer-input).

### P4 — Built-in emoji fallback

Status: backlog. Add a resident font-renderer fallback path: prefer the current
font's supported glyphs/sequences, then use a built-in emoji set bundled in
System resources. All native Toolbox clients share the service and assets.
Preserve classic MacRoman rendering, measurements and byte-based APIs.

Support sequence-aware lookup and text/emoji presentation, consistent drawing
and measurement, and grapheme-aware native TextEdit behavior. Pin Unicode and
asset versions/provenance; define supported coverage, color/monochrome behavior,
clipping, sizing and missing-sequence policy. Cache within bounded memory and
keep redraw local. See [the font service plan](../system/src/toolbox/quickdraw/fonts/README.md#planned-built-in-emoji-fallback)
for public SDK acceptance and performance gates. This is a Unicode extension,
separate from closing original System 7 font-fidelity differences in R4.

### P5 — System-owned Apple menu

Status: persistent leftmost Apple title implemented, including empty/cleared app
bars and resource-based app sections recognized by title rather than menu ID.
System owns that title alongside its right-hand application menu. Shared System
entries and their dispatch, plus the remaining contribution APIs and legacy
projection, remain backlog. The remaining design adds contribution APIs and
System entries, tracking and dispatch. Foreground changes and cleanup must
preserve the title while restoring/removing the app section.

Compose legacy Apple-title MENU/MBAR content without duplication, preserving
app-visible IDs, item indices, mutation and selection behavior through the legacy
projection. Verify original behavior rather than hardcoding the conventional
menu ID. Cover empty/cleared bars, app switching, crash cleanup, ownership and
bounded menu-cache invalidation. See [the Menu Manager plan](../system/src/toolbox/menus/README.md#persistent-apple-title-and-remaining-system-menu-work).

### P6 — Control panels and kernel extensions

Status: backlog. Provide resource-based Rust control-panel applications and a
`no_std` Rust System-extension SDK whose modules can execute in the kernel.
Keep panel UI on application threads; expose typed resident configuration,
service/hook registration and versioned JSON preferences through Rust interfaces.

Load independently relocatable extension resources from the blessed installation's
`System Folder/Extensions`, with drivers discovered separately in
`System Folder/Drivers`. Keep their discovery paths and package entry contracts
distinct while sharing dependency resolution. Load extensions with checked
ABI/dependencies, explicit startup phases, staged registration and
an extensions-disabled boot mode. Use shared generation-tagged identities and
synchronized ownership. Initially pin modules until reboot; unloading, workers
and SMP each require their lifecycle/scheduler gates. Kernel panic reporting is
fatal and distinct from application crash cleanup.

Preserve a separate original `cdev`/`INIT` compatibility track, including hybrid
files and translated legacy callbacks. Add sample extension/panel, Extensions
Manager, public SDK tests and private boot/failure/performance probes. See
[the control-panel and extension plan](SYSTEM-EXTENSIONS.md).

### P7 — Full networking stack

Status: backlog. Build a resident, thread-capable host stack with loopback and
virtio-net, IPv4/IPv6, ARP/NDP/ICMP, UDP/TCP, addressing/routing, DHCP, DNS and
multicast/discovery. Native apps use owned Rust networking services through the
Toolbox; a minimal platform adapter later backs `std::net`. Evaluate an existing
Rust protocol engine against an explicit conformance matrix before selecting it.

Depend on real timer/blocking/wakeup, ownership and bounded-buffer foundations.
Add a JSON-backed Network control panel, TLS/HTTPS with entropy/clock/trust gates,
and separately verified MacTCP/Open Transport and AppleTalk compatibility tracks.
Keep interrupt and packet work bounded, preserve UI responsiveness under load,
and test through public SDK services plus controlled private QEMU peers.
See [the staged networking plan](NETWORKING.md) for protocol scope, dependencies,
device/extension boundaries and acceptance criteria.

### P8 — Rust standard library

P8 begins when the minimum platform/Toolbox foundation is present; completion
of the entire Toolbox is not a prerequisite. Generic IPC and networking may follow later; they do not gate the first std
applications. Full std is the default native
application destination, with no_std retained for kernel/low-level modules and
transitional bring-up gates.

Native SDK async/await is a base application requirement. Deliver a std-compatible
event-driven executor, runtime-independent futures and optional Tokio integration
over the same completion source. See [ASYNC-SDK.md](ASYNC-SDK.md) for sequence,
thread-affinity rules and acceptance. Drag promises are the first slice; guest
std/Tokio availability is not implied by that slice.


Status: backlog; existing no_std apps, resident services and one-hart app/event
wait scheduling provide foundations, not a std port. Inventory compiler/target
selection and all std platform backends, pin a maintained sysroot patch/build,
and bridge normal Rust startup to resource-based loading.

After the agreed storage/driver/clipboard sequence, prioritize S0/S1 application
build/runtime support: std becomes the default for
every native userspace app, including Finder and all apps/*. Remove no_std,
no_main, handwritten entry and panic-handler macros as each app migrates. The
platform's default std panic hook calls the System-owned bomb dialog without
app setup, including when take_hook restores the default. All six existing apps
must build/launch through that path; a single opt-in sample is insufficient.

Deliver pointer allocation/runtime first, then worker threads/TLS/blocking,
general files/streams/paths/child processes and P7 sockets. Keep the std backend
on a core-only ABI/transport path independent of mac-toolbox; both call the same
resident services. Start with panic_abort and explicit unsupported operations;
unwinding/backtraces and wider ecosystem support retain separate gates. SMP is
not required for the first std threads. See [the module map and staged plan](RUST-STD.md)
for current evidence, service requirements, Finder migration and guest tests.

### P9 — System Fonts folder

Status: backlog. Build a Fonts folder inside the blessed System Folder and load
installed font suitcases/files through the resident Font Manager. Resolve the
active installation by identity; provide native folder lookup and legacy
FindFolder behavior. Keep System-bundled UI/emoji fonts as dependable fallbacks.

Preserve source-local resource/family associations, handle duplicates explicitly,
and share validated payloads across apps. Start with boot-time catalog discovery
and restart-required installation; live refresh/removal needs snapshot lifetime
and generation/cache gates. Include resource-based Finder icons, original install
behavior, public SDK enumeration/rendering and private boot tests. See
[the font installation plan](FONTS.md#fonts-folder-and-discovery).

### P10 — OpenType and color fonts

Status: backlog. Support standalone OpenType fonts/collections, TrueType and
CFF/CFF2 outlines, native GSUB/GPOS/GDEF shaping and variable-font selection.
Add COLR/CPAL v0/v1, CBDT/CBLC, sbix and a bounded font-SVG profile in measured
slices; publish table/encoding coverage rather than inferring support from a
filename. Integrate shared text runs, metrics, TextEdit and P4 emoji fallback.

Keep classic MacRoman rendering and APIs stable. System owns parsers, shapers,
rasterizers and bounded shared caches; apps use Rust Toolbox interfaces. Gate
each format with provenance-backed fixtures, public SDK drawing/measurement,
malformed-input and color/monochrome tests, and responsiveness/cache limits.
See [the OpenType/color plan](FONTS.md#opentype-outlines-layout-and-variations).

### P11 — Terminal and CLAP applications

Status: backlog. Add CLAP as the file type for terminal-only std applications,
with a resource-based Terminal APPL. System validates a Terminal session before
command entry; Finder routes CLAP opens to Terminal, and each invocation creates
its own process. Commands have normal argv/cwd/streams/status and child lifecycle.

Terminal's versioned JSON preferences define the base environment. New sessions
snapshot it; children inherit effective parent state with explicit overrides,
without mutating siblings or retroactively changing running processes. Define
terminal input/emulation, resize, bounded scrollback, pipes/readiness and shutdown.
See [the Terminal/CLAP plan](TERMINAL.md) for build, ownership and guest gates.

### P12 — Hosted Rust toolchain

Status: backlog. Run cross-built rustc, Cargo, linker and packaging tools as CLAPs
inside System RISC, then build and run local CLAP/APPL programs. Requires P8/P11,
general workspace storage/resource-fork writing, process/pipe support, measured
RAM/image limits and P13 protection. Qualify native LLVM/runtime dependencies,
build scripts and proc-macro module loading explicitly. Offline builds precede
P7-backed registry/TLS fetching; compiler self-hosting is a later gate. See
[the hosted-toolchain plan](HOSTED-TOOLCHAIN.md).

### P13 — Process isolation

Status: backlog. Replace trusted shared M-mode application execution with
per-process virtual address spaces and user/kernel privilege separation. Protect
APPL and CLAP memory, stacks/TLS, kernel/devices and syscall buffers; safely route
app-provided resource callbacks and terminate faulting processes through System
reporting. Retain generation/owner checks alongside hardware protection.

Implement one-hart boot/MMU/trap/loader/copy-user foundations, then lifecycle/IPC
and P1 TLB/reclamation concurrency. Kernel extensions and drivers remain trusted
in-kernel code, outside the app isolation boundary. See
[the process-isolation plan](PROCESS-ISOLATION.md) for actual-fault guest gates.

### P14 — Loadable kernel drivers

Status: backlog. Keep the bootstrap block driver built into System; extract input,
network, display and other backends into in-kernel P6 driver modules. Require the
DRVR package marker and a build-generated DABI contract metadata resource before
loading. Validate driver/kernel API versions, architecture, entry layout,
required services/features and device-class contracts before entry/device claims.
Install/discover native driver packages in the blessed `System Folder/Drivers`;
reserve `System Folder/Extensions` for System extensions. Image tooling creates
both directories, and loaders reject misplaced classes before entry.

Define typed class interfaces, shared generation-tagged identities, bounded
interrupt/deferred work and a quiesced ROM-to-System block handoff. Native and
legacy DRVR entry contracts remain separate. Modules stay resident until reboot
initially; reset/removal still requires request/DMA cleanup. See
[the driver plan](DRIVERS.md) for metadata and extraction acceptance.
After P15, prove the initial interface with P16/P17 storage drivers before
extracting input, network or display; bring in the required P6 loader subset.

### P15 — HFS-B and early system migration

Status: **native boot/default-image cutover verified, 2026-10-20; follow-on filesystem APIs remain**. Follow the supplied
[HFS-B specification](HFS-B.md). HFS-B is
a separate native UTF-8, dual-fork format alongside unchanged classic HFS, not an
extension of HFS's name keys or date fields. Move native workspaces to HFS-B early,
before P8's broader filesystem workloads and P12 hosted tools depend on classic
HFS limits. HFS-B must be bootable and becomes the required normal startup format
after the cutover gate below; a workspace-only slice is transitional. All five
high-level spec decisions are settled. Timestamps are unsigned 64-bit
microseconds since 1904-01-01 UTC, with checked native/std/legacy conversions.
Stored names use NFC UTF-8 with an inclusive 255-byte limit per component after
normalization. Search
uses canonical caseless matching with Unicode default folding independent of
locale, no Turkish-i tailoring, and preservation of case in the stored NFC name.

Native RRF1 resource forks now use checked u32 payload offsets. Startup is
qualified with a System fork over 5 MiB, and indexed/streamed SDK reads exercise
offsets past 16 MiB. The ordinary eager application loader still has a 4 MiB
selected-payload budget; this is separate from fork size and addressability.
The page-backed heap stage replaces that fixed cap with checked per-domain
allocation budgets, retaining bounded/targeted reads for large resources. Include
the separate whole-data-file writer limit in the native write-service audit.
Do not merely raise one constant or accept an unbounded fork-sized allocation.
Keep malformed/budget rejection, boot and UI/performance acceptance gates.

1. Freeze a byte-accurate versioned layout and test vectors before image creation:
   signature (proposed HB), volume header, allocation bitmap, catalog/extents/
   attributes trees and fork records. Encode the decided locale-independent,
   non-Turkic Unicode folding and case-preserving behavior with a pinned data
   version, NFC storage and canonical caseless lookup, plus the inclusive
   255-byte post-normalization UTF-8 stored-name limit and unsigned 64-bit
   microseconds from the 1904 Mac epoch. Include HFS-B blessing/startup metadata.
   HFS-B uses its own NFC contract rather than HFS+'s decomposed storage. Keep
   classic HFS serialization unchanged.
2. Build the shared safe no_std mac-hfs-b crate and Linux create/format/check
   tooling, then resident/ROM mount/read adapters. Linux tools, kernel and the
   future System RISC Filesystem app reuse the same codecs, formatter, validator,
   name rules and filesystem algorithms through a common block interface.
   Keep platform I/O and UI outside the crate; extract shared block traits rather
   than importing classic HFS parsing. mac-build/xtask image creation uses this
   same implementation. The native std app uses public Toolbox device/volume
   services and cannot block the early crate/Linux/kernel milestone. See
   [tooling and reuse requirements](HFS-B.md#shared-format-crate-and-tooling).
   Use distinct signature dispatch, bounded checked B-tree/extent parsing and
   the same File/Resource Manager services. Native clients receive full UTF-8 names and metadata;
   expand the current 128-byte catalog name buffer and second/legacy-width date
   transport rather than silently truncating names or microseconds. Preserve
   existing 64-bit fork lengths/offsets throughout all backend operations.
3. Implement writable files, both forks, allocation/tree growth, metadata and
   clean unmount/remount with explicit write/flush-error and dirty-volume policy.
   Use bounded page/path updates rather than rebuilding every tree per edit.
   Journaling and live HFS conversion remain outside this first spec. Reported
   error rollback is not a claim of power-loss atomicity.
4. Add ROM HFS-B header/catalog/extent reads and blessed-folder discovery. Load
   System/Finder from the selected HFS-B installation, then hand off the same
   volume identity to System's writable backend. Do not depend on HFS files or
   optional disk-loaded modules to read the primary boot volume.
   ROM displays original Happy Mac artwork after boot validation and the original
   missing-disk artwork if no bootable installation is found; capture both before
   System entry and verify display ownership/handoff without duplicate drivers.
5. Cut over image/test tooling once formatter validation, HFS-B-only cold boot,
   all shipped app launches, fork/metadata remount persistence and malformed/
   write-failure gates pass. New normal builds require an HFS-B startup/workspace
   volume; reject incompatible media clearly instead of silently falling back.
   Route std/native files, resources, Fonts and CLAP/APPL handling through shared
   services. Finder lists both formats and Get Info identifies them.
   Volume Get Info reports the mounted filesystem and actual whole-number version
   through public Toolbox metadata, plus capacity/free space. Copy/import
   preserves forks/metadata; live in-place HFS conversion is not required.
6. Canonical acceptance lives in crates/mac-toolbox/tests through public SDK
   services: mixed HFS/HFS-B mounts, 200-byte UTF-8/emoji/CJK names, 255/256-byte
   post-NFC create/rename boundaries including multibyte/decomposed input, case lookup
   and CAFÉ/café collision/ID-equivalent lookup with stored spelling preserved
   in NFC across remount/locales, composed/decomposed collisions, canonical
   combining-mark ordering and default non-Turkic I/i behavior,
   precise timestamp round-trips and checked epoch/2040/overflow projections,
   fork round-trips, actual file I/O crossing 2/4 GiB boundaries, malformed media,
   write failures and clean remount. Test long MacRoman legacy-name truncation
   separately from unrepresentable names returning BAD_ARGUMENT; verify ID/index
   reachability without ambiguous short-name aliases. Verify blessing, boot-to-
   runtime identity and selection with both disks present; retain original HFS
   compatibility tests. Any retained HFS boot check is an explicit transitional/
   recovery profile. Cross-check Linux-created volumes in the guest and guest-
   modified/formatted volumes with the Linux tool; require codec/vector parity,
   read-only/exclusive access and incomplete-format failure tests. Use private
   generated disks and prune them after checks.

Bootable HFS-B and the default-image cutover are required deliverables of this
early workstream. Keep the built-in block driver filesystem-neutral;
host-directory mounts remain a separate workstream.

### P16 — StuffIt archive volumes

Status: classic v1 read-only native driver verified on HFS-B and writable/read-only
host exports. Driver-owned file associations and 16/32 color/mono icons accompany
the generic Finder open path; a missing driver supplies neither. Native tests cover
double-click, both fork reads, data copy-out, source pins and stale IDs. Full
two-fork copy transactions, original Macintosh comparisons and guest cancellation
during deferred reads remain open. Preserve both forks and Macintosh metadata; decode on demand with bounded
caches and work. Standard File, Resource Manager and copy-out use the existing
public services. Qualify an explicit format/codec matrix, source leases, eject,
integrity, unsupported formats and stale IDs. See [the archive-volume plan](ARCHIVE-VOLUMES.md).

### P17 — Vhost shared-directory volumes

Status: implementation in progress after P15; the checkpoint above records the
verified private-guest subset and remaining gates. Provide a separate vhost filesystem/block-device
driver pair: VM transport/device I/O below, guest catalog and fork semantics
above, each with DRVR/DABI contracts. Directory exports need directory operations
alongside the transport's actual block-export interface; do not model host paths
as sectors. Qualify the actual Linux/native Windows QEMU transport before choosing
it. One versioned host file stores both guest forks and metadata, transparently
to apps. Share its codec with Linux packing/validation tools; specify portable
names, stable identity, external-change conflicts, root confinement and checked
write/flush guarantees. See [the vhost plan](HOST-FOLDERS.md).

### P18 — Virtual host clipboard extension

Status: required System-extension proving milestone in the memory/model module
integration stage, following the working P16/P17 driver-interface gates. Add a
P6 kernel extension that negotiates a compatible host clipboard bridge and
shares clipboard changes in both directions through the resident Scrap service.
Absent/unsupported bridges leave the local clipboard working. Start with Unicode
text, preserve legacy conversions at their trap boundary, and use per-change
notifications with revision/origin tracking to prevent echo loops and stale
updates. Qualify real Linux/native Windows host paths and bounded asynchronous
transfers. See [the clipboard extension plan](HOST-CLIPBOARD.md).

## Planned application tooling

### A1 — ResEdit resource-editor extensions

Status: backlog. Replace static viewer dispatch with independently linked
`RSSC` editor resources embedded in ResEdit itself, following the original
`@TYPE` editor naming convention. Every built-in viewer uses the same contract
as custom editors. Provide a versioned module/host ABI, an idiomatic Rust layer,
and a sample editor that can be added without browser source changes.

Reuse relocatable native code resources and public Toolbox services; preserve
whole-pane editor ownership, bounded repainting and inspection without executing
the inspected file's code. Read-only extensions can ship before Resource Manager
write-back. Original 68k extensions separately need translation and a host-API
adapter. See [ResEdit's extension plan](RESEDIT.md#planned-resource-editor-extension-api)
for lifecycle, compatibility and acceptance requirements.

## Evidence and limits

The current consolidated regression report is `results/system7/toolchain-checks.json` outside the repository. It establishes accepted subsets, not missing selectors. The original comparison profile remains English/Roman System 7.6.1, one 640×480 display. Native UTF8 string/Pascal conversion pairs do not establish original A-line ABI or legacy record layout. The native scheduler supports one hart and fixed executable reservations; that is separate from blocking, Rust std, protected memory and multi-hart support.

Known font differences remain explicit: hinted TrueType has 294 differing RGB pixels in 256,000; the style sample has 585 in 230,400. Matching all bundled resources and measured metrics does not close those differences.

A defensible API completion percentage requires a version-filtered per-selector inventory with native behavior, original captures, public SDK tests, RV64 results and classic adapter status recorded separately. The current family inventory and green subset reports do not provide that denominator.

Source anchors:

- Memory: [system/src/toolbox/memory/README.md](../system/src/toolbox/memory/README.md).
- Resource Manager: [system/src/toolbox/resources/manager.rs](../system/src/toolbox/resources/manager.rs).
- QuickDraw / PICT / icons: [system/src/toolbox/quickdraw/service.rs](../system/src/toolbox/quickdraw/service.rs).
- Color / Palette / devices: [TOOLBOX-PLAN.md](../TOOLBOX-PLAN.md).
- Fonts / Roman text: [system/src/toolbox/quickdraw/fonts/README.md](../system/src/toolbox/quickdraw/fonts/README.md).
- Events / clocks: [EVENTS.md](../EVENTS.md).
- Windows / WDEF: [system/src/toolbox/windows/README.md](../system/src/toolbox/windows/README.md).
- Controls / CDEF: [toolbox/controls/src/lib.rs](../toolbox/controls/src/lib.rs).
- Menus / MDEF: [crates/mac-toolbox/tests/README.md](../crates/mac-toolbox/tests/README.md).
- List Manager / LDEF: [TOOLBOX-PLAN.md](../TOOLBOX-PLAN.md).
- TextEdit: [system/src/toolbox/textedit/README.md](../system/src/toolbox/textedit/README.md).
- Scrap: [system/src/toolbox/scrap/README.md](../system/src/toolbox/scrap/README.md).
- Dialogs / alerts: [toolbox/dialogs/src/lib.rs](../toolbox/dialogs/src/lib.rs).
- Files / HFS persistence: [system/src/toolbox/files/README.md](../system/src/toolbox/files/README.md).
- Standard File / aliases: [TOOLBOX-PLAN.md](../TOOLBOX-PLAN.md).
- Processes / application life: [THREADING.md](../THREADING.md).
- Apple events / core IPC: [TOOLBOX-PLAN.md](../TOOLBOX-PLAN.md).
