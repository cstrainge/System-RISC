# Core Toolbox remaining work

Assessment baseline: `320eee1` (native application-launch milestone).

17 deliberately grouped workstreams, not an API count or a completion percentage. Status describes the current tested native slice; effort is a qualitative planning judgment about the remaining contracts.

All 17 workstreams have remaining work. Ten have usable tested native slices, four have narrower foundations, and three groups have no public manager. These are capability groups, not equally weighted API counts. None is signed off for full original System 7 parity.

Core date/time, fixed/fract arithmetic, queue and truthful Gestalt/availability utilities are cross-cutting R1 work rather than separately counted manager groups. General Rust std support remains a separate platform effort alongside these services.

The practical next target is a native document application that can open, edit, save and reopen files, with proper resource dialogs and idle waits. Sound, printing, networking, full non-Roman scripts, disk formatting and VM/device compatibility are separate expansion tracks; they are not hidden inside this core count.

| Workstream | Current native surface | Remaining effort | Important gaps |
| --- | --- | --- | --- |
| Memory | Usable tested slice | L | Pointer/master-pointer ABI, heap zones, pressure-driven purge/compact and grow/purge callbacks. |
| Resource Manager | Narrow foundation | L | Open/current/close resource files, mutable chains, resource handles and detach/purge; add/write/update after fork writes. |
| QuickDraw / PICT / icons | Usable tested slice | L | Complete port/recording lifetimes, grafProcs, advanced transfers/patterns, indexed worlds and remaining PICT semantics. |
| Color / Palette / devices | Narrow foundation | XL | Actual Palette Manager, GDevice/depth lifecycle, color matching, indexed conversions and palette animation. |
| Fonts / Roman text | Usable tested slice | L | Measured TrueType/style pixel differences, fractional spacing/kerning, text policy, Roman word/collation/formatting services; native emoji fallback (P4). |
| Events / clocks | Usable tested slice | M | WaitNextEvent sleep/mouse regions, scheduler wakeups, timer/caret/double-click policy, suspend/resume and extended pointer input (P3). |
| Windows / WDEF | Usable tested slice | M | Full query/visibility/hilite/behind/refcon lifecycle, custom definition callbacks, regions and classic records. |
| Controls / CDEF | Narrow foundation | L | Owned control objects/lists, full properties/lifetime, popup/user controls, action callbacks and keyboard focus. |
| Menus / MDEF | Usable tested slice | M | Dynamic editing, hierarchy/popups/scrolling, complete icon/style variants, custom lifecycle, flashing and System-owned Apple menu (P5). |
| List Manager / LDEF | No public manager | L | Cells, owned data, selection, scrolling, clicks/double-clicks, drawing and LDEF callbacks. |
| TextEdit | Usable tested slice | L | Editable-dialog integration, styled text, other fonts/alignment, word selection/autoscroll, TEHandle/private scrap ABI. |
| Scrap | Usable tested slice | M | Disk Scrap.dat, original counted-MacRoman/private TextEdit scrap and process suspend/resume integration. |
| Dialogs / alerts | Narrow foundation | L | General DLOG, editable/user items, tab focus, item mutation, modeless/filter callback contracts and alert stages. |
| Files / HFS persistence | Usable tested slice | XL | File creation and fork write/resize, catalog/extents growth, flush/recovery, full metadata operations and eject/unmount. |
| Standard File / aliases | No public manager | L | Open/save dialogs and filters/hooks, alias records/resolution across moves and ownership of replies/resources. |
| Processes / application life | Usable tested slice | L | Blocked/wakeup states, document/quit parameters, native workers, relocation, legacy cooperative Thread Manager (P2) and segments. |
| Apple events / core IPC | No public manager | L | Descriptors/coercion, handlers, open-document/print/quit events, replies/timeouts and process routing. |

M means a bounded extension of a working manager; L means substantial lifecycle/behavior work or a new manager; XL means a broad storage/device foundation. These are relative planning judgments, not engineer-weeks or measured completion fractions. Parallel work and new original-system findings can change the order.

## Delivery sequence

### R1 — Service lifetimes and waits

Depends on: the current ABI6 baseline.

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

### P1 — Multi-hart execution (SMP)

Status: backlog; additional harts currently park in the boot ROM. This is a
separate platform workstream alongside the Toolbox delivery sequence.

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

Status: backlog. System always inserts exactly one leftmost Apple menu, alongside
its persistent right-hand application menu. Apps contribute resource-defined
custom items through a Rust Toolbox interface; System retains its own entries,
tracking and dispatch. Restore contributions on foreground changes and clean
them up on exit without removing the Apple menu.

Compose legacy Apple-title MENU/MBAR content without duplication, preserving
app-visible IDs, item indices, mutation and selection behavior through the legacy
projection. Verify original behavior rather than hardcoding the conventional
menu ID. Cover empty/cleared bars, app switching, crash cleanup, ownership and
bounded menu-cache invalidation. See [the Menu Manager plan](../system/src/toolbox/menus/README.md#planned-system-owned-apple-menu).

### P6 — Control panels and kernel extensions

Status: backlog. Provide resource-based Rust control-panel applications and a
`no_std` Rust System-extension SDK whose modules can execute in the kernel.
Keep panel UI on application threads; expose typed resident configuration,
service/hook registration and versioned JSON preferences through Rust interfaces.

Load independently relocatable extension resources from the blessed installation
with checked ABI/dependencies, explicit startup phases, staged registration and
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
