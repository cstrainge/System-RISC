# Toolbox implementation and conformance plan

This plan targets application-observable **System 7.6.1 Toolbox behavior** through
the public Rust SDK. It covers the remaining managers as well as unfinished
contracts in existing managers. It does not describe a complete Macintosh OS,
complete Finder, or a working general-purpose 68k application translator.

The first implementation slice is the **Memory Manager**, driven by new original
Mac probes. Memory implementation and the aggregate test harness are separate
work streams. This document is an inventory and dependency plan; proposed APIs,
fixtures and milestones below are not claims of implemented functionality.

## Compatibility profiles

Freeze profiles before adding expected results. A header declaration alone does
not establish that an API exists in the installed reference system.

| Aspect | Original reference profile | Initial native profile |
| --- | --- | --- |
| System | Installed English/Roman System 7.6.1 and its actual extension/resource inventory, identified by retained hashes | Independently built Rust System and Finder, current versioned native service ABI |
| Machine | Existing isolated QEMU `q800`, `m68040`, 128 MiB, pinned Quadra-650 ROM; exact launch command in `working/quickdraw-reference/command.json` | QEMU `virt`, RV64IMAC, one hart, 128 MiB; the actual platform backends and capability bits |
| Display | One 640×480 indexed-eight-bit screen with captured CLUT/gamma projection; separate original 32-bit GWorld cases where appropriate | One 640×480 RGB32 framebuffer; explicit original-device presentation profile where required, not an assumed gamma formula |
| Input/text | Pinned US keyboard map, Roman script/Mac Roman resources and original installed fonts; record VM/extensions, original mouse/event semantics | Virtio keyboard and absolute tablet, Mac event codes; host Ctrl-to-Command mapping is a documented host adapter |
| Storage | Disposable clone of the reference HFS disk; record blessing, Finder metadata, forks and installed resource files | Read-only mounted classic HFS, native block callbacks, generated 500 MiB boot image; no implied HFS write support |
| Execution | Actual original 68k manager calls and original application-defined procedures | One foreground native application today; native definition resources use the public SDK; no general 68k execution |
| Unavailable devices | Record the actual reference device/extension capabilities, rather than assuming them | No general sound, printer, network, serial, ADB/NuBus/SCSI or power-management API promised by the initial machine profile |

The original display profile is specifically an indexed-device oracle. It cannot
stand in for every Macintosh monitor, direct-color device, printer or script.
Additional depths, monitors, scripts, language editions, audio devices and printer
backends require separate named profiles and original evidence.

Compatibility defaults must not silently adopt modern behavior. Larger text
limits, Unicode-first operations, additional transfer modes, alternate color
management, preemptive scheduling or a PDF printer can be useful **opt-in native
extensions**. They need separate capability/profile names and tests. No extension
may replace the baseline behavior to make a comparison pass. Existing ergonomic
Rust choices, such as zero-based enumeration or UTF-8 slash-path helpers, require
explicit mappings to the original semantic operation; they are not original ABI
representations.

Native scheduling is an explicit architecture requirement: every application
runs in its own OS-scheduled thread, independently of event-loop yields. Classic
Thread Manager cooperative contexts are confined to one application's OS thread.
The direct-call bootstrap is a current gap, not an acceptable final scheduling
model. [THREADING.md](THREADING.md) defines context ownership, synchronization and
the scheduler, application-launch and multi-hart acceptance gates; foundational
manager changes must preserve that path from M1 onward, not defer all ownership
work until M9.

## Required delivery loop

Every contract follows this order:

1. **Inventory and capture original evidence.** Identify original names,
   signatures, selectors/trap modifiers, availability, resources, state,
   ownership and callbacks. Capture controlled original execution, including
   success, documented failure and state transitions. Record the input sequence,
   system/ROM/resource/toolchain hashes, initial state and capture method.
2. **Encode Rust tests through the public SDK.** Put the operation sequence and
   expected observations under `crates/mac-toolbox/tests`. Missing SDK operations
   are an explicit unimplemented contract, not a test that passes because the
   manager is absent. Add typed bindings needed to express the original operation.
3. **Implement the resident service.** Keep manager state, algorithms, caches,
   resources and device policy in System. Bindings remain transport and safe
   ownership wrappers. Implement the smallest complete captured contract first.
4. **Run the same contract against RV64.** Cross-build the public test application,
   enter the real gate, exercise actual loaded definition resources and compare
   results. Host-only tests or a mocked gate cannot establish this step.
5. **Integrate into Finder or another native application.** Reuse the SDK service;
   remove duplicated application algorithms where appropriate. Add combined
   scenarios so individually correct managers also work together.

A feature ledger should carry a stable case ID, original API aliases, profile,
fixture/provenance paths, public Rust entry points, native ABI contract, expected
bytes/errors/state/pixels/callback trace, and these independently visible states:
`inventoried`, `captured`, `SDK test encoded`, `resident implemented`,
`host verified`, `RV64 verified`, `application integrated`, and
`classic adapter verified/deferred`. An implemented function or imported asset
cannot advance the other states automatically.

## One aggregate public-API test suite

**All canonical Toolbox behavior and original-compatibility Rust tests belong in
`System7/crates/mac-toolbox/tests/`.** Keep manager-specific files there, for
example `memory.rs`, `resources.rs`, `quickdraw.rs`, `fonts_scripts.rs`,
`windows_controls.rs`, `menus_dialogs.rs`, `textedit_scrap.rs`, `files_disks.rs`,
`events_processes.rs`, `devices_media.rs`, `classic_adapter.rs` and
`composition.rs`, with shared harness/fixture support. These are proposed
organization names; the harness work stream owns the implementation.

Test bodies must use `mac_toolbox::System` and the public userspace manager
bindings. They must not construct resident `Canvas`, `Region`, `FontManager`,
`WindowManager` or allocator objects, call private resident helpers, or assert on
private fields. Bootstrap/capture support may launch System or enter its exported
service boundary, but cannot substitute a direct-engine backend for SDK calls.
Public offscreen-world readback is suitable for many graphics assertions;
compositor tests may obtain the actual framebuffer through the harness/QMP.
Raster comparison assertions and acceptance decisions belong in Rust. Python
capture/launch helpers can remain orchestration and evidence-generation tools.

Use one case catalog for host integration and the RV64 test application. A
transport mock is useful for payload layout/rejection tests, but is not a manager
behavior oracle. Existing parser/algorithm safety unit tests may remain auxiliary.
Move their app-observable compatibility assertions into this aggregate suite;
do not maintain a second competing private-engine acceptance suite.

Each applicable case must verify:

- **Values and errors:** exact specified result, short/partial transfers,
  documented failure, state after failure, and domain error codes. If Rust maps an
  original error into an enum, publish and test the mapping without losing the
  original distinction. Do not flatten unrelated failures into one success-like
  result.
- **State and ownership:** object identity/lifetime, retained buffers, selection,
  current port/file/zone/resource chain, disposal, locking, dirty state and update
  delivery. Test operation sequences, not just isolated calls.
- **Wire data:** original big-endian records and packed fields where exposed;
  native ABI sizes/alignment/counts/buffer limits and rejection separately.
  Original 32-bit addresses and RV64 addresses are not compared numerically;
  address relationships and exposed semantics need an explicit representation
  mapping.
- **Pixels:** exact RGB/mono/indexed results for the named device profile and
  declared initial background, origin, clip, font and state. Keep every compared
  pixel; do not erase mismatches, widen thresholds or regenerate goldens from the
  Rust renderer. Park the cursor outside a compared region rather than hiding its
  damage with a mask. Record any deliberate ownership crop explicitly.
- **Callbacks:** invocation count/order, arguments, return values, retained
  context, allowed nested calls and state after return. A callback parameter in a
  Rust signature is not sufficient evidence of compatibility.
- **Time/devices:** controlled virtual clocks where possible, exact epoch/wrap
  and deadline relationships, and explicit reference-derived timing bounds only
  where the observation is inherently asynchronous. Host wall-clock jitter is
  not permission for a blanket tolerance.

Undefined original memory accesses are not ordinary conformance requirements.
Use documented failure inputs and isolated diagnostic probes; record an original
crash separately. Safe Rust rejection and classic fault behavior need separate
contracts. Quantified font or device mismatches remain visible partial/failing
coverage, not pixel-parity passes. A green accepted subset must report the
unimplemented and excluded portions of the target inventory.

## Current inventory and remaining work

This survey was taken at the start of the Memory Manager work on 2026-10-18.
Source is changing in parallel; the linked ledgers and generated reports retain
more precise evidence boundaries. In particular, the Memory namespace inspected
here was a scaffold and must not be described as finished while its first slice
is being built.

| Area | Present foundation | Remaining application-visible contracts |
| --- | --- | --- |
| Memory | System allocator; empty public manager namespace at survey time | Pointer/handle lifecycle, resize/clear/copy, state bits, locks, relocation, purge/compact/pressure, zones, grow/purge procedures, temporary/system heaps, queries and errors |
| Virtual memory/cache utilities | No application VM/MMU compatibility service | Hold/lock/logical-to-physical/cache operations and feature queries; explicitly pin original VM state. Native shared-address M-mode execution is not protected memory or a VM Manager. Add a separate backend/profile when these contracts are implemented. |
| Resources | Shared read-only application→System/font search; names/IDs, enumeration, attributes, partial reads, pinned load/release | Resource-file lifecycle/current file/search-chain mutation; resource handles, loading/purging/detach; add/change/remove/write/update; failure and file/heap interactions |
| QuickDraw | Stateful shapes, regions, polygons, Boolean bitmaps, masks/scroll/fill helpers, direct-RGB worlds, retained replay | Complete port/recording lifecycle, custom grafProcs, text state, coordinate extremes, indexed conversions, PixPat, arithmetic/hilite/dither modes, full masks/damage/GWorld options and handle semantics |
| Color/Palette/Display | RGB rasterization and measured original palette presentation | RGB16 precision, Color2Index/Index2Color, search/complement procedures, GDevice lists/depth/DeviceLoop, gamma, CLUT ownership, Palette Manager reservation/animation and multiple devices |
| Fonts/QuickDraw text | Original bitmap and sfnt resources, family associations, metrics, native hint-aware drawing | Remaining scaler/style pixel differences, fractional widths/spacing/kerning, policy and fallback, font-cache memory behavior, complete original text procedures and callback semantics |
| Scripts/text utilities | Mac Roman conversion in existing paths | Script Manager, international resources, word/line boundaries, collation/case, number/date formatting, keyboard layout switching, non-Roman scripts, Text Services/input methods and dictionary services |
| Events/time input | Resident queue, masks/peek, activation/update input, key mapping and ticks; thin next-event API | Full public posting/peeking/flushing, key maps, double-click/caret policy, WaitNextEvent sleep/mouse regions, suspend/resume, event ownership, high-level events and timer/task interactions |
| Windows | Standard loaded WDEF0 variations, z order versus active focus, tracking, damage/exposure and retained content | Broader lifecycle/query/hilite/visibility/refcon/behind semantics, Window Manager port/regions, custom WDEF contracts, picture/color windows and classic records |
| Controls | Loaded button CDEF0 and scrollbar CDEF1, typed scrollbar draw/hit/value tracking | General control objects/lists, lifecycle, bounds/value/range/refcon/title, check/radio/popup/user controls, keyboard focus, action procedures, resource creation and complete tracking semantics |
| Menus | Resource-installed MENU/fmnu objects, loaded native MDEF0 layout/drawing/hit, enable/check state, captured pointer tracking and keyboard command routing through the public SDK | Dynamic insertion/deletion, all item icons/marks/styles, arbitrary MDEF lifecycle, hierarchical/scrolling/pop-up variants, original flashing, per-app menu ownership and classic trap adaptation |
| Lists | No List Manager public service | Cells/data/selection/scrolling, LDEF, click/double-click, drawing modes, resource/procedure lifetimes and control integration |
| TextEdit | No public manager | Mono/styled edit records, classic text limits, CR handling, selection/caret/hit testing, line layout/scroll, edit operations, style scraps, hooks and drawing/activation integration |
| Scrap | No public manager | Multiple scrap flavors, clear/append/read, private/system scrap conversion, memory/disk state, suspend/resume and TextEdit/dialog clipboard behavior |
| Dialogs/alerts | Extracted visual resources; public namespace scaffold | DLOG/DITL/ALRT creation, item state/text, editable/static/user items, defaults/cancel/tab focus, modal/modeless filters, events, alert stages/sounds and disposal |
| Files | Mounted HFS volumes, catalog/CNID/path operations, read-only data/resource forks, seek/close | Create/delete/rename/move, write/truncate/extend, allocation/extents/catalog persistence, metadata/locks/permissions, flush/unmount/eject, working directories, async parameter blocks and exact failures |
| Aliases/Standard File | No public managers | Alias records/resolution/update under move/rename; original open/save dialogs, filters/hooks, replies, directory selection and resource ownership |
| Disk tools | Host image builder and guest read path | Writable block contract, disk initialization/verification, partition/volume operations, formatting/blessing and a native disk utility using the SDK |
| Desktop/Finder services | Application-owned retained desktop layer; real read-only volume/folder browser | Desktop Manager database versus compositor distinction; file associations/comments/icon caches, saved Finder state, writable desktop metadata, drag/drop, alias/application/document launch and integration |
| Processes/segments | Boot loader starts one native Finder; loaded native definition resources | Process identity/partitions, cooperative scheduling, foreground/background ownership, launch/quit/document parameters, suspend/resume, desk accessories, segment/code-resource lifecycle and cleanup |
| Apple events/IPC | No public manager | Descriptor ownership/coercion, handlers, core open/print/quit events, sending/replies/timeouts, object model, PPC/interapplication services and Edition Manager |
| Utilities | Geometry helpers and narrow classic trap dispatch | Gestalt truthfulness, fixed/fract/SANE/decimal/bit/string utilities, queues, date/time/measurement, PRAM/settings, packages, shutdown/restart, system errors and trap-table APIs |
| Sound/media | No general public audio backend | SysBeep, channels/commands/resources/buffers, asynchronous callbacks, mixing/format conversion; input, speech and components when the selected profile supplies them |
| Printing | No public Printing Manager/device | Print records/job/page lifecycle, page setup/job dialogs, printer resources/drivers, PICT/comments/spooling, cancellation/errors and device-specific output |
| Devices/drivers | Virtio block/input, RAM framebuffer and serial diagnostics are machine backends | Device Manager open/close/prime/control/status/kill/async completion, DCE/driver lifetimes; serial/SCSI/ADB/slots/power compatibility or explicit backend/profile exclusions |
| Other 7.6.1 services | Imported resources are not implementations | Help/Balloon Help, Component Manager/control panels, Translation Manager, Drag Manager, notifications, VBL/deferred tasks, networking/AppleTalk/Open Transport and extension-specific services after availability inventory |

The public facade is `crates/mac-toolbox/src/lib.rs`; thin manager bindings are
under `toolbox/`; implementations are under `system/src/toolbox/`. Existing
coverage detail: [QuickDraw](../analysis/quickdraw-coverage.md),
[fonts](system/src/toolbox/quickdraw/fonts/README.md),
[PICT](../analysis/quickdraw-pictures.md), [icons](../analysis/icons-reference.md),
[desktop](../analysis/desktop-compositor.md),
[File Manager](system/src/toolbox/files/README.md), and
[architecture/classic adapter](ARCHITECTURE.md).

## Dependency milestones and exit criteria

These are dependency milestones, not calendar estimates. Independent branches
can run after their prerequisites; no unmeasured completion dates are implied.
The current raster/UI prototypes need not be discarded while foundations mature.

| Milestone | Depends on | Deliverable and exit criteria |
| --- | --- | --- |
| **M0 — Oracle and aggregate harness** | Current boot/service gate | Freeze profiles; capture manifests and feature ledger; public SDK host/RV64 harness works; migrate the existing accepted comparisons below. Reports distinguish missing, partial, platform-excluded and passed contracts. Runs do not alter the user's visible VM or original golden disk. |
| **M1 — Memory Manager, first active slice** | M0 acceptance seam | Original probes and public SDK tests for pointer/handle basics, clear/resize/data preservation, flags/locking, errors and copies pass on host and RV64. Then controlled pressure, relocation/purge/compact, zones and callbacks in separately captured sub-slices. No full-Memory claim after allocation basics alone. |
| **M2 — Resource ownership and procedure contracts** | M1 basics; existing fork reads | Real resource-file/chain lifecycle, current-file behavior, resource handle loading/pinning/purge/detach and memory interaction; native callback/definition lifetime rules. Mutable resource writes wait for M7. Custom definitions run through SDK callbacks with tested nesting/errors. |
| **M3 — Events, clocks and core utilities** | M0; M1 where objects/tasks allocate | Queue ordering/filtering/removal, event state, sleep/wake and controlled ticks; fixed/fract/date/epoch/queue/Gestalt basics. Timer/deferred/notification ownership expands with M9. Exact event traces and errors pass through the SDK before UI relies on them. |
| **M4 — Graphics, color, fonts and Roman text contracts** | M1–M3 as applicable | Finish port/recording/bitmap/device/color/font obligations; resolve recorded glyph/style differences and text state. Add Roman Script/Text Utilities primitives needed by editors. Exact accepted pixels, record bytes, widths and callbacks pass in indexed and direct-color profiles. Non-Roman coverage is a named follow-on profile, not an implicit success. |
| **M5 — Window/control/menu/list objects** | M2–M4 | Full captured lifecycle and state for windows and control objects; Menu Manager/MDEF and List Manager/LDEF. Tracking, capture/cancel, focus, disabled states, scrolling, custom procedures and resource-driven instances pass in combined applications. Static menu labels cease to stand in for a Menu Manager. |
| **M6 — Scrap, TextEdit, dialogs and Standard File** | M1–M5 | Scrap + mono/styled TextEdit first; then DITL/DLOG/ALRT behavior, filters, editable items, defaults/cancel and original open/save dialogs. Verify selection/caret/CR/style data, cross-editor paste, clipping/scrolling and callback traces. Alert sound is complete only with its audio contract or an explicit unavailable-device result. |
| **M7 — Writable files, forks and resource persistence** | M1–M3; existing HFS reader; writable native block contract | Scratch-image create/write/resize/rename/move/delete, both forks, catalog/Finder metadata, free-space/accounting, flush/remount and failures. Complete Resource Manager writes on this foundation. Cross-check with original operations and an independent HFS reader after restart; readonly media and failed writes preserve state. |
| **M8 — Aliases, disk utility and persistent desktop data** | M5–M7 | Alias behavior across moves; Desktop Manager database/services, Finder state and file-type/icon associations; disk initialization and a native utility using public APIs. Verify dismount/remount, blessing/bootability, fork preservation and error recovery on disposable disks. |
| **M9 — Processes, application launch and interapplication contracts** | M1–M3, M5; resources/files | Each application on its own actual OS-scheduled thread; partitions/ownership, foreground switching, kernel preemption/blocking/wakeup, launch/document/quit parameters and cleanup. Legacy cooperative Thread Manager contexts stay inside each app. Add Apple-event descriptors/handlers/core events and relevant notifications/tasks. Finder launches separate apps through services, not an internal function call. Desk accessories/segments and legacy code execution have separate adapter gates. |
| **M10 — Sound, printing and general device services** | M1–M3; M5–M6 for UI; M9 for process ownership | Independently captured device contracts: sound buffers/commands/callbacks/sample results, print records/dialogs/page/job/spool/cancel/errors, Device Manager queues and completion. Baseline absent hardware is truthful; supported optional device profiles have concrete backends and acceptance oracles. No silent successful no-ops. |
| **M11 — Remaining scripts, components and system integration** | Relevant earlier milestones | Additional language/script/text-service profiles, VM/cache/physical-memory utilities, help, components/cdevs, translation, drag/drop, Edition Manager, network/communications and remaining utilities/extension APIs. Version-filter the inventory, capture real installed implementations, and close their dependency-specific test gates. |

Memory and resource basics unlock much of the rest, but a complete printer or
SCSI compatibility layer need not block a Roman TextEdit dialog. Conversely,
rendering a dialog-shaped rectangle does not satisfy M6. Apple's documented
initialization dependencies explicitly place graphics/fonts/windows/menus and
TextEdit ahead of the Dialog Manager.
[InitDialogs](https://dev.os9.ca/techpubs/mac/Toolbox/Toolbox-391.html)

### M5 animation and interaction gate

“All controls and window definitions” means a version-filtered inventory, not
only the currently loaded WDEF0 and CDEF0/1. Include rounded WDEF1, every installed
standard control family, definition messages/variations, custom resource dispatch,
and all captured active/inactive/disabled/pressed/selected states. Each retained
reference case requires exact pixels through the aggregate public SDK and real
resident loaded resource. Missing definitions remain explicit open contracts.

Capture interaction sequences as well as final images: drag/grow dotted-outline
frames without live document movement, acceptance/cancellation and reentry,
background activation and Command-drag, close/zoom tracking, original zoom
transitions if present, control press/leave/reenter, repeat/scroll/thumb motion,
menu tracking and focus. Compare framebuffer sequences, geometry/state changes,
event/callback ordering and tick-relative transitions. Freeze original captures
before accepting animation algorithms; do not insert modern easing or live resize.
Retained redraws must remove old outlines exactly, preserve clipping/occlusion and
leave no stale pixels after cancel. Native desktop bounds/device rate are named
profile policies; arbitrary classic limit/slop rectangles and callbacks need
separate contracts before being advertised.

[DragWindow](https://dev.os9.ca/techpubs/mac/Toolbox/Toolbox-245.html),
[GrowWindow](https://dev.os9.ca/techpubs/mac/Toolbox/Toolbox-250.html), and
[TrackBox](https://dev.os9.ca/techpubs/mac/Toolbox/Toolbox-253.html) describe these
tracking obligations. Current private manager regressions and the isolated QMP
input runner are auxiliary until equivalent contracts run through the aggregate
SDK test fixture; they do not waive the M0 migration requirement.

The first migrated window interaction slice is now implemented by
`crates/mac-toolbox/tests/window_interactions.rs`: a public SDK-only application
on a private native guest matches the original drag/grow masks, and real Finder
checks validate held/release/cancel/reentry behavior and whole-frame restoration.
This establishes those captured states only; it does not close M5's other
window definitions, all control families, arbitrary callback/bounds contracts or
exhaustive temporal animation sampling.

### First Memory Manager probe/test inventory

Capture these before selecting internal allocator behavior:

- Pointer versus handle allocation, clear variants, empty handles versus zero-size
  requests, odd/even sizes, zeroed versus unspecified bytes and disposal.
- Size queries and grow/shrink results, retained prefixes, failure preservation,
  recover/duplicate/append/copy/block-move operations and overlap behavior.
- Lock/unlock and repeated calls, get/set state, purgeable/resource flags, empty
  and reloaded handles. Do not assume a classic lock bit behaves like a Rust
  nested borrow count.
- Controlled heap pressure and fragmentation: successful/failed resize, compact,
  purge, reserve, move-high and queries; fixed block stability and handle identity
  across relocation. Use a specified zone capacity and allocation history, not
  incidental free bytes from two different operating-system builds.
- Current/system/application zones, grow-zone and purge-warning callbacks,
  temporary memory and cleanup. Observe callback arguments/order/nesting and
  post-callback error/state, in later sub-slices where needed.

Safe Rust handles/leases must preserve observable semantics without exposing a
movable raw pointer as a stable borrow. The classic adapter additionally needs
original master-pointer/zone/address conventions. Neither a generic Rust allocator
nor an arbitrary token table proves those contracts on its own.
[Memory Manager](https://dev.os9.ca/techpubs/mac/Memory/Memory-56.html)

## Migrate the existing visual evidence through the aggregate SDK

The old evidence remains valuable. The migration changes **how the native side
is exercised**, not the original expected results.

| Existing evidence | Aggregate public-API replacement |
| --- | --- |
| Original shapes, 267 oval-region encodings, 88 line point sets, transfer modes and region pages | Draw/construct with public graphics, regions and worlds; read pixels/bytes through public APIs and assert in Rust. Include port state, origin, clipping, overlap and failure preservation. |
| WDEF variations and active/inactive/pressed states | Create real SDK windows using loaded resource procedures; drive activation and input through the harness; compare actual compositor pixels. Do not set private WDEF state directly. Preserve the separately captured ownership boundary where application content is independent. |
| CDEF button/scrollbar reference images and hit/value results | Public control objects/state calls and real loaded CDEFs; input tracking + callback trace; test multiple windows/origins, disabled ranges and cancellation. |
| Font/metric/style/reference text pages | Public Font Manager queries and text drawing into public worlds/ports. Preserve the exact resource/scaler/device profile and visible partial cases. |
| Icon/resource/PICT goldens | Public Resource Manager lookup, icons/picture playback and drawing; preserve palette, mask, origin and resource-chain contracts. Test resource eviction/reload once implemented. |
| Python/QMP desktop and exposure checks | Keep launch/capture support, but put canonical behavioral/pixel assertions and original fixture loading in aggregate Rust tests. Exercise the public desktop scene/window/event/file services and actual retained composition. |
| Original filesystem/metadata and persistence observations | Public volume/catalog/fork/resource/alias calls on disposable media; reopen/remount and independently inspect resulting bytes/metadata. Host hfsutils success alone does not prove the guest File Manager. |

Required combined cases include: custom WDEF/CDEF calling graphics and fonts;
multiple clipped windows with covered content and exposed desktop; resource
handles under memory pressure while repainting; TextEdit in a scrolling window
with controls, menus, selection/caret and clipboard; modal dialogs with filters,
editable fields and nested events; save through Standard File and Resource Manager
then remount; and two applications exchanging open-document/quit/paste events
with foreground/background changes. Each scenario must assert final state and
errors as well as the visible result.

## Native Rust parity and classic 68k compatibility are separate gates

The native SDK uses safe ownership types, native payloads and an RV64 gate.
The classic adapter must additionally marshal original registers/Pascal stacks,
return slots, trap modifier bits, packed big-endian records, 24/32-bit addressing
where relevant, low-memory globals, handles, procedure pointers/UPPs and callbacks.
There are many trap constants already declared in `mac-abi`; declaration is not
implementation. The current classic dispatcher implements narrow geometry,
text/font, event and cursor paths, not the whole native Toolbox surface.

For each manager, track original-function→Rust-operation mappings and original
trap/selector→adapter status independently. Add adapter tests under the same
aggregate root through its public compatibility entry point. They must verify
register preservation, stack adjustment, aliased arguments, bounds/failure
atomicity and callback roundtrips against original traces. A native callback
cannot execute a 68k address by casting it to a Rust function pointer. Execution,
translation of code resources and cross-architecture procedure dispatch remain
explicit prerequisites. CFM/Mixed Mode and PowerPC application compatibility need
their own availability and execution plans; they are not supplied by native ELF
loading.

## Inventory sources and completion boundary

The pinned local interface inventory is:
`.toolchains/retro68/rootfs/Retro68-build/toolchain/multiversal/CIncludes/Multiverse.h`
(SHA-256 `bdd1eca59dcc4df745018b1ab25180e082926c93af25c109e8140dd6482ec626`).
It contains original family sections and register/stack/trap declarations, but
also later or optional APIs. Filter individual entries against the actual 7.6.1
reference, availability queries and original execution. For example, presence of
Navigation/Code Fragment/Mixed Mode headers is not a promise that all such APIs
belong to this initial reference profile.

Apple's primary manuals, mirrored here, define the family/dependency inventory:
[Memory](https://dev.os9.ca/techpubs/mac/Memory/Memory-2.html),
[More Macintosh Toolbox](https://dev.os9.ca/techpubs/mac/MoreToolbox/MoreToolbox-2.html),
[Text](https://dev.os9.ca/techpubs/mac/Text/Text-2.html),
[Files](https://dev.os9.ca/techpubs/mac/Files/Files-2.html),
[Processes](https://dev.os9.ca/techpubs/mac/Processes/Processes-2.html),
[Operating System Utilities](https://dev.os9.ca/techpubs/mac/OSUtilities/OSUtilities-2.html),
[Devices](https://dev.os9.ca/techpubs/mac/Devices/Devices-2.html), and
[Sound](https://dev.os9.ca/techpubs/mac/Sound/Sound-2.html).
TextEdit and Scrap are distinct managers with explicit shared dependencies;
[TextEdit](https://dev.os9.ca/techpubs/mac/Text/Text-40.html) and
[Scrap Manager](https://dev.os9.ca/techpubs/mac/MoreToolbox/MoreToolbox-109.html)
provide their original contracts. The Desktop Manager database is likewise
separate from drawing a desktop surface.
[Desktop Manager](https://dev.os9.ca/techpubs/mac/MoreToolbox/MoreToolbox-483.html)

A milestone is complete only for its declared profile and accepted contracts.
An aggregate suite passes only those contracts it actually exercises. Neither
that pass, a visually similar Finder window, imported System resources nor a list
of trap numbers establishes whole-System-7.6.1 compatibility.


## Cross-cutting text, date and packaging contracts

Every newly implemented human-text API must have paired native UTF-8 and legacy
Pascal/MacRoman trap entries. Conversion lives only at System's trap boundary;
Rust apps use `mac-toolbox` string interfaces. ABI4 established the initial
string-bearing families; ABI5 adds resource strings and alert substitutions.
Original A-line adapters retain their historical calling
conventions. Aggregate userspace tests compare decoded results and pixels and
reject invalid UTF-8, malformed Pascal input and unrepresentable output.

Every date-bearing API must accept2026. Keep classic dates unsigned1904-seconds
at disk/trap boundaries and native dates signed64-bit values, with checked
conversion and no two-digit-year pivot assumptions. Current filesystem metadata,
MacBinary build dates and public timestamp conversions have2026 regressions;
ABI5 wall-clock reads also have public native/guest coverage. Future calendar
and formatting APIs need equivalent public tests before use.

Application `vers` resources come from Cargo package versions, including workspace
inheritance. Finder uses only the aggregate Toolbox facade at runtime. PNG
image decoding is resident QuickDraw functionality, with bounded input and
transparent composition tests; applications must never link the decoder.


## Preferences contract

All system preferences use versioned JSON documents. Public Rust access is typed;
the JSON encoding is a storage format, not a replacement for a service API.
System and per-application preference namespaces follow the execution context.
Legacy settings/PRAM/preference adapters use that store where applicable, while
presenting the original ABI to callers. Writers serialize changes and use an
atomic replacement or equivalent recoverable transaction, so concurrent app
threads cannot overwrite each other's edits or leave truncated JSON. Invalid or
unknown-version documents produce explicit errors with a documented recovery
policy. Writable storage and these transactions remain M7 prerequisites; the
current bounded catalog/Trash write API does not yet persist preferences.
