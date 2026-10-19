# Resident System and Toolbox clients

Applications use the public `toolbox/*` crates for Rust interfaces. These crates
marshal requests through `mac-traps`; they contain no resident manager state,
rendering engine, font strikes or heap. `mac-toolbox` groups them behind a
loader-initialized `System` handle. `mac-abi` has no dependencies and defines
the versioned C layouts, protocol constants and shared value types.

The native dispatch path is:

```text
Finder / definition resource
  -> public Toolbox binding
  -> mac-traps typed request
  -> loader-supplied gate -> RV64 ECALL
  -> System machine trap entry
  -> system::toolbox::<manager>
```

The RV64 gate uses `a7` for the operation, `a0` for the request pointer and `a1`
for its byte size. ABI 5 supplies one gate and capability bits in the 24-byte
`SystemServices` structure. [FORMAT.md](FORMAT.md) describes the exact contract.
Applications and System must be rebuilt together when this ABI changes.

## Required threading model

Each application, including Finder, must run on its own actual OS-scheduled
thread. Kernel scheduling is independent of application event/yield calls.
Legacy cooperative Thread Manager contexts exist only inside their application's
OS thread and never replace the global scheduler. Per-app resource/memory/event/
port state and per-thread trap/callback state must be selected by trusted
execution identities. The current direct-call single-app bootstrap does not yet
satisfy this requirement. See [THREADING.md](THREADING.md) for the shared-state
audit, synchronization rules and scheduler/application-thread exit criteria.

## Resident implementation

`system/src/toolbox` contains QuickDraw, windows, controls, events, resources,
files, menus, dialogs, memory, scrap and textedit modules. The implemented managers own the
rasterizer, bitmap/TrueType font manager, window state, retained drawing lists,
offscreen worlds, cursors, icon and PICT decoders, event queue and shared read-only
resource search, HFS catalog/Trash mutations and bounded handle Memory Manager.
The resident Menu Manager tracks resource-defined menus and invokes MDEF0 for
sizing, drawing and hits. The initial Dialog Manager interprets ALRT/DITL and
calls WDEF1/CDEF0, with modal input routing. The shared Scrap Manager owns bounded
binary-flavor records and native UTF8 text behind a separate nonwaiting guard.
The Event Manager separates its128-entry native application queue from the
captured20-entry low-level OS queue; physical input state is observed before
window/menu/dialog routing. Monochrome TextEdit owns generation-checked editor records, fallible UTF8 editing/layout and clipped retained drawing. Native clipboard edits stage changes while holding the desktop then Scrap guards; no borrowed text escapes. Full classic managers remain incomplete.

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
call without reborrowing the runtime. System/application resource files and font
storage are published once before application entry and borrowed immutably;
readonly resource queries also bypass the mutable desktop borrow, including
queries nested inside a definition. Resource data moved into the font manager
remains visible through the System resource scope. The current trusted M-mode environment has
shared memory; these checks are ABI validation, not process isolation.

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

ABI5 extends BootInfo to104bytes with firmware disk-info/read/write/flush callbacks
and startup volume identity. DiskInfo carries capacity, read-only state and the
driver icon resource identity. System serializes firmware access through its I/O
owner guard. The File Manager owns mounted-volume IDs, catalog traversal,
independent fork marks and mutation policy. Its bounded HFS engine rebuilds
catalog/thread/index/map records within existing fork reservations. New Folder,
Trash, Put Away and recursive Empty Trash persist to the actual device; failed
writes/flushes trigger rollback and failed rollback poisons further mutations.
This is not a crash-atomic journal. Native data/resource-fork writing, arbitrary
rename and catalog-fork growth remain future work. Neither SDK nor Finder links
the HFS engine. Finder aggregates actual per-volume Trash directories in a
virtual window and keeps selection identities as (volume,CNID) pairs.

The desktop is a retained application scene below windows and the menu bar.
Finder begins a staged desktop update, draws through ordinary QuickDraw, then
commits it atomically. Desktop activation deactivates a folder without changing
window order. CDEF1 owns scrollbar geometry, hit tests, drawing and thumb-value
mapping in its own executable resource; the Control Manager only retains and
dispatches it. See `analysis/desktop-compositor.md` at the repository root.
