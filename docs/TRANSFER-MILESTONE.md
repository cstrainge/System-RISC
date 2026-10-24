# Complete external-volume workflows

This milestone covers six external-volume workflows and the resource-editor
follow-on architecture. Each mount driver must implement its complete required
contract before the corresponding workflow can be considered complete.

## Acceptance targets

1. Open supported native applications and associated documents directly from
   external volumes; mount supported StuffIt archives through real Finder opens.
2. Complete writable provider operations: both forks, folders, metadata, rename,
   move, delete and persisted icon/folder state. Immutable archives implement
   the full read contract and reject writes before side effects.
3. Shared, bounded recursive transfer transactions preserve both forks and
   metadata. Moves retain identity within a volume; cross-volume moves verify
   destination copies before removing sources. Escape cancels the whole batch,
   rolls back its changes and reports any cleanup failure honestly.
4. Finder uses those services for drop-time Option-copy/Command-move and Trash
   policy, with proper managed progress/static-text controls and bounded painting.
5. Public SDK plus actual Finder gates exercise startup ↔ external transfers,
   archives → writable destinations, errors/collisions/cancellation/disconnect,
   reboot persistence and immediate interaction recovery.
6. Application icons accept compatible document sets from resource declarations.
   Drop opens file references in that exact application file/instance without
   moving source files; incompatible sets reject during negotiation.

Volume-to-Trash eject is also required (2026-10-22): unmount writable host and
read-only archive volumes, preserve their contents, save writable folder state,
close their Finder windows only after acknowledged teardown, and reject startup
or busy mounts without disturbing windows/selection. Eject has separate native
Drag Toolbox intent; it is never a recursive move of a root into Trash.

## Current findings

2026-10-22: all six targets have implementations and passing focused acceptance.
The canonical run passed the host, build, boot and filesystem matrix, then
stopped in Res Edit with resident heap exhaustion (6,912-byte allocation;
51,376 bytes free, largest hole 3,584). The user deferred Res Edit to its planned
rewrite. The remaining canonical groups pass in separate private captures after
the regression fixes below; this is not a claim that the entire canonical suite
is green. The continuation sections
below retain historical findings that have since been resolved.

- Native public-SDK host acceptance passes recursive transfers in both
  directions, both forks, property edits, namespace operations, readonly
  refusal, cancellation with date restoration, RAII Drop, real app-exit cleanup,
  Trash/Put Away/Empty, immediate retry and cold-boot identity/metadata retention.
- Physical Finder acceptance passes external application launch, compatible
  document drops, incompatible/readonly negotiation, drop-time modifiers,
  file-to-Trash, host RW/RO eject and startup eject refusal. Separate physical
  archive eject verifies the source archive's digest remains unchanged.
- StuffIt public-SDK acceptance passes the supported classic/5 codecs, nested
  mounts and recursive two-fork archive copies to native/writable host volumes.
- Both native disconnect directions pass. Cleanup ownership remains resident
  while healthy native storage and subsequent transfers remain usable. The
  transport poisons an incarnation after five seconds without packet progress,
  including when virtconsole supplies no close notification.
- Finder recovery/placement acceptance passes named collision alerts, immediate
  selection recovery, group moves, Escape, saved positions and cold boot. Its
  drag-motion trace asserts no file/font calls or unrelated scene repaint.
- Rust library documentation has regenerated under `docs/api`.

## Verification record (2026-10-22)

- Canonical run: 538 host tests, allocated >4 GiB public-SDK fork, optimized
  installation, boot and disk integrity, compiled architecture boundaries,
  six native host-module cases and ten StuffIt cases pass. The large-fork
  fixture uses sparse zero elision, not 4 GiB of physical host allocation.
- Remaining guest groups pass separately: menus, persistent Trash, native writes,
  deferred close, editable Get Info, events, clipboard, TextEdit pixels/documents/
  navigation, process ownership/preemption, app switching/icons, retained updates,
  latency, cursors/busy watch, app panic recovery, selection/scrolling, Transfer
  incident recovery, remote Get Info and physical Finder drags.
- Final fixes preserve desktop selection across modal dialogs, avoid virtual
  Trash position writes, and allow pointer tracking alongside automatic layout
  persistence. New storage menu commands wait for that owned background group.
  The synchronous icon-move SDK convenience now uses the same group protocol on
  native and provider volumes; 56 focused SDK cases pass after consolidation.
- The application-menu pixel fixture derives the right-aligned popup edge from
  its actual frame; Text Edit's shorter name legitimately changes menu width.
  Get Info fixtures preserve icon positions across rename, and wait for bounded
  folder patches instead of requiring obsolete full redraws.
- Res Edit's System allocation failure remains open, with heap/register/frame
  evidence in `results/system7/resedit-ui-1341735` outside this repository. Its
  test is retained. One earlier run exceeded the virtio FLUSH deadline; isolated
  rerun and the subsequent combined storage matrix pass unchanged. No uncertain
  write retry or deadline extension was introduced.

Logs outside Git: `results/system7/filesystem-canonical-final-3.log`,
`filesystem-final5-*.log`, `filesystem-application-ui-final.log`,
`filesystem-final4-get_info_ui.log`, `filesystem-final3-{clipboard,typing,
 cursor_graphical,panic_graphical}.log`, `filesystem-sync-layout-sdk.log`,
`filesystem-boundaries-commit.log`; earlier focused logs cover both endpoint
 disconnects, archive eject and recursive transfers. This is named workflow
 coverage, not exhaustive hardware fault injection or full classic Toolbox
 compatibility. Provider-volume eject is covered; startup eject is refused and
 nonstartup block-device eject remains unsupported.

The follow-on specification review is recorded in
[RESOURCE-EDITOR-PLAN.md](RESOURCE-EDITOR-PLAN.md). The rewrite itself is deferred;
Text Edit performance and display-option work is next, in its own commit.

## Milestones

- [x] Real external opens, document delivery and archive-format regression.
- [x] Complete provider mutation contracts and persistence.
- [x] Transfer engine and Finder copy/move/cancellation UI.
- [x] Application drag targets and complete end-to-end acceptance matrix.
- [x] Record the combined resource-authoring, editor and extension follow-on design.

Use private optimized guests, preserve the visible VM, retain reports outside
Git and prune obsolete media at every check boundary. Passing smaller gates
closes only the named milestone, never the whole driver contract.

## Historical implementation batch (superseded by current findings)

This section records implementation work before the combined verification
pass. Current findings above describe its subsequent qualification.

The pending tree adds field-selective provider metadata transactions, optional
identity-keyed annotations, mixed plain/container host-tool export, delayed
Finder folder-state persistence, and an owned rename/move publication protocol.
The rename intent retains the actual publication independently of a directory
sync failure; cancellation must query it before reporting an unpublished result.

Read-only folders permit presentation-only icon rearrangement. Native Drag
Manager `IconLayout` intent is distinct from a filesystem folder target and binds
the entire offered reference set to its original volume/parent. Finder retains
those positions in memory for the mount session, paints only old/new item bounds,
and writes no archive metadata. Cross-folder/cross-volume writes into immutable
media remain rejected. This is view state, not a writable filesystem capability.

The same batch replaces `mac-formats::Error` prose classification with typed
budget/allocation, missing, translation-required and malformed causes. Details
remain verbatim diagnostics. The allocation-free `mac-code-resource` wire error
remains separate; its adapter preserves reader causes and classifies the codec's
own structural failures as malformed without inspecting their messages.
Public-SDK process fixtures now contain deferred acceptance assertions for
oversized/corrupt resource forks and missing/legacy WDEF rejection with immediate
valid-window retry. The focused format/Resource Manager subset passed before the user reiterated the implementation-first instruction. The broad workspace attempt did not complete (generated linker output exhausted disk space). Final native/canonical verification remains deferred; obsolete debug outputs were removed to recover space.

All six targets stay open: complete namespace/delete and provider identity
contracts, bounded recursive transfers, rollback/cancellation and managed copy
UI, document associations/application targets, the full unhappy-path matrix and
panic recovery still require implementation and/or final verification.

## Failure and recovery gates

Every workflow includes unhappy paths with useful error-message assertions.
Preserve the actual cause (including archive version/method) through driver,
File Manager and Finder. A failure must leave selection, events and subsequent
operations usable, release protocol/source/open ownership and undo provisional
changes. Rollback failures quarantine affected resources and expose recovery
steps. Application panics are contained; System must be able to clean up and
restart Finder. A System panic shows diagnostic details and a Restart button
through a minimal panic-safe drawing/input path, without resuming services.

StuffIt completion requires multi-version external-format support, including
classic pre-5 and StuffIt 5. The real Aldus archive uses method 15 (Arsenic) for
all nine nonempty forks; a stored-only SIT5 parser would still be incomplete.

## Verified diagnostic / event-recovery milestone

2026-10-21: archive failures retain typed causes and version/method bytes through
DRVR, owned jobs, File Manager, SDK and resource-based Finder alerts. The native
SDK gate verifies truncation, header/fork checksums, encryption and unsupported
version/method messages, source release after rejection, and immediate reuse.
The actual Finder gate uses raw host files, dismisses a version-99 alert, observes
a newly added valid archive and opens it without switching windows. Removing the
driver still removes its file handler and icon association.

The gate exposed an existing refresh starvation bug: a retained folder returned
without acknowledging Update while its cache was unpublished. A private trace
recorded 294,514 Update deliveries and 3,534,272 menu calls. Native
Windows::validate now acknowledges existing exposure with no drawing, allocation
or scene change; Finder uses it during refresh. Successful open captured 4 Update
deliveries / 260 menu calls; error/dismiss/host-refresh/retry captured 6 / 356.
The tests bound both counters. The public SDK also checks active-transaction
rejection, repeated acknowledgement, later damage and forced-slot stale IDs.

Validation: seven focused host tests, four isolated optimized QEMU tests, rebuilt
installation, boundary check and generated API docs. This is a diagnostic and
refresh recovery milestone; the six workflow completion boxes above stay open.
Finder restart and System Panic/Restart UI remain implementation requirements.


## Verified classic-revision-2 / StuffIt-5 and Arsenic slice

2026-10-21: shared no_std/forbid(unsafe_code) mac-sit dispatches classic revisions
1/2 and StuffIt 5 by header. Version 5 validates both header CRC layers, bounded
variable headers, parent/child counts and directory sentinels; it preserves both
forks, supported Finder metadata, comments and timestamps. Mounted Get Info
reports the container's filesystem version. Driver FASS rules include SIT!,
SITD, SIT2 and SIT5 and disappear with the package.

The resumable Arsenic decoder handles arithmetic coding, MTF, inverse BWT, RLE,
randomization, multiple blocks and embedded CRC32. CPU-only work yields without
requesting more source bytes. Decoder requests remain under 512 bytes; large
arithmetic state uses fallible owned storage instead of being copied on every
callback or enlarging simple codecs. Small forks cap workspace by their known RLE bound.
The actual reference uses 512 KiB blocks; the module's declared/private heap is
4 MiB, with a matching larger link reservation. Shared generic admission limits
separate code bytes (2 MiB), runtime reservation (6 MiB), heap (4 MiB) and stack
(64 KiB), with specific errors and negative tests. Total machine-arena bounds
still apply independently.

Validation: 16 parser/codec tests; eight driver callback/cancellation tests;
module contract tests; public error-range and package-budget tests; seven private
optimized QEMU gates, rebuilt image, boundary/workspace checks and API docs.
The actual reference archive opens in Finder, and all nine nonempty forks pass
CRC validation through the public SDK on writable and read-only host exports.
The original source archive and regular visible VM were left untouched.
Project-owned synthetic fixtures retain reproducible independent-writer provenance.

Remaining: other compression methods, same-provider nested mounts,
provider mutation/copy contracts, external native app/document launch, transfer
UI/application targets, and the recovery requirements above. This does not close
any of the six full-workflow completion boxes.

## Method-13 slice

The shared safe/no_std decoder supports all five standard LZ-Huffman codebooks
and dynamic shared/separate tables with bounded, resumable parsing and a fixed
64 KiB history. It validates meta operators, canonical trees, lengths and CRCs;
no complete fork is buffered. Decoder/request moves remain below 512 bytes.
Project-owned vectors include an independent writer and maximum history/distance
cases. Bad checksum or malformed payload state is discarded without poisoning
the volume or its other fork; cancelled lower reads are acknowledged before retry.

Validation: 20 parser/codec tests, ten driver callback tests, an optimized private
image and three private guest gates (public SDK plus real Finder opens of raw
classic-revision-2 and StuffIt-5 host archives). SDK coverage includes 17 fixture
variants on the startup volume, valid/corrupt host sources in both export modes,
informative errors, immediate retry and source release. Remaining codec, nested
mount, mutation, transfer, document/application delivery and panic recovery gates
above remain open; this slice does not close the complete driver contract.

## Finder geometry regressions included in the acceptance matrix

- SuperPaint's top folder carries kHasBeenInited with point (-30208,-10238).
  Coordinates placing the whole icon outside the usable canvas, or collapsing
  its rectangle at i16 saturation, are treated as unplaced in the Finder cache.
  Original archive metadata is retained; read-only mounts use a session placement.
- Folder extents include the complete icon and wrapped caption on all four sides.
  Scroll ranges cover those extents plus the current viewport. They contract after
  scrolling without changing the view origin; once everything fits the bars are
  inactive. One pixel of label overflow still enables the corresponding bar.
- Scrolling uses a masked viewport transaction, cached geometry and managed CDEF
  tracking. It leaves the information band and unrelated windows alone. No-change
  scrolling does no drawing; unchanged partial-update bars are not resubmitted.
- The upper-right zoom box fits cached contents and chrome within the desktop,
  with resident standard-state tracking/restoration rather than unconditional
  full-screen sizing. Cancelled tracking preserves both content and user state.

The real SuperPaint gate now opens the visible top-level folder and captures its
children, rather than accepting only a mounted volume window. Native Finder SDK
checks cover one-pixel label overflow, both axes, range expansion/contraction,
no-change work and bounded viewport commits; pure geometry tests include invalid
archive positions, full signed-range mapping and screen-constrained fitting.

The extended selection stress gate exposed a stack overwrite, rather than heap
exhaustion: inlining combined the cache publication scratch arrays with the
selection callback's large frame. Separate non-inlined persistence and snapshot
callbacks keep those temporary lifetimes in distinct frames. The original
30-stage / 4,000-outline selection gate and the new scrolling gate pass together
without increasing the application stack or System heap budgets.

Validation: seven host layout tests, resident zoom tracking, public SDK negative
and stale-window checks, optimized private Finder selection/scrolling, actual
SuperPaint navigation and native zoom/restore captures, workspace/boundary checks,
rebuilt release installation and regenerated API docs. The six full workflow
completion boxes remain open.

## Application-loader identity prerequisite

Process Manager now stores applications in the shared generation-tagged facility.
Finder receives a real process identity at boot, replacing literal application-1
assumptions in desktop access, drag registration, lifecycle notifications and crash
policy. Reusing a retired process slot rejects its former ID; foreign manager IDs
and values with extra upper bits also fail. The native SDK and Pascal conversion
boundary share the resident validation. This is a prerequisite for provider-backed
loading, not completion of external application/document launching.

Private trace format 2 records full process identities in a bounded counter map;
captures discover actual registered/launched IDs. Fixture preparation records its
live owner before building child applications so media pruning cannot remove a
prepared disk before QEMU starts.

Finder's immutable resources now have one owner in the image cache. Bootstrap
formerly cloned the entire fork into a second unused resource-file field; removing
that duplicate saves 584,856 bytes in the private three-application capture, with
the same 8 MiB System heap. Resource lookups require the actual cached image.

ResEdit retains its resource panes across unchanged exposure/application switches.
Only a document/view or content-size change rebuilds them. Save/export updates
only a changed status footer through an owned masked transaction; a cancelled
Save As changes no document pixels. The full graphical workflow verifies all
viewers, both save paths, cancellation, new folders, resize, switching and lossless
JSON rebuild. Flags/dates are compared against the captured live catalog through
the public SDK, including Finder's legitimately initialized icon metadata.

Validation: shared-facility lifetime/namespace tests, crash-service host tests,
media-cleanup tests, native/Pascal process reuse and ownership, real application
menus/relaunch, graphical panic recovery, event tracing, Finder drag recovery,
SuperPaint navigation and the complete ResEdit workflow. Optimized installation,
workspace/boundary checks and generated docs/api pass. The six external-volume
completion targets above remain open.


## Host object identity and coherent-open prerequisite

Host wire/HDIR contract 2 describes an opaque object incarnation separately from
its revision and editable guest dates. The Linux companion qualifies identity
from device, inode and nanosecond birth time where available; unavailable birth
information is explicit, not fabricated from a path or modification time. Live
pins and the shared generation table still protect session object IDs. A sorted
key index replaces an fstat of every previously pinned object on every Describe.

Plain-file opens compare the pinned read revision against both the subsequent
Describe and the cached catalog source. A host rewrite with unchanged size and
restored modification time rejects the stale open; cleanup acknowledges its
close, and normal watch invalidation permits immediate retry on the same catalog
ID. Read-only guest exports have the same coherent read behavior. This is not yet
persistent provider metadata or completion of writable-volume operations.

Validation: shared protocol, Linux companion and both driver component suites;
private optimized public-SDK gate over writable/read-only exports, including
source-change rejection, useful error text, watch refresh and retry; optimized
installation, boundary checks and generated docs/api. Old internal protocol
versions are rejected with an explicit rebuild message; no dual decoder remains.


## External application loading

The Process Manager routes launches before acquiring desktop state. File Manager
owns provider catalog/open/read/close work across scheduler waits, retaining a
private generation-tagged fork mark through acknowledged close. Cancellation and
caller exit drain the operation; failed close quarantines its mark and reports
failed cleanup. Replays resume their owned operation even if the mount becomes
unavailable, rather than abandoning a pending read behind an availability check.
Desktop activation/spawn begins only after resources are owned and storage closes.

A shared safe resource loader reads checked map/payload ranges in at most 64 KiB
steps, with the existing map/count/4 MiB payload budgets. Block-backed launches
use the same decoder. It preserves resource names/attributes, skips fork padding,
and never retains a second complete fork buffer. Malformed fork data reports the
public damaged/malformed-data error rather than treating it as a bad caller pointer.
Existing running instances activate without rereading their application.

Public SDK acceptance launches the same native application from writable and
read-only host exports, verifies its own resource chain and process/file identity,
activates it again without a duplicate process, and checks exit cleanup/unmount.
The read-only launch first enters through the legacy Pascal conversion trap.
A damaged external application rejects without a process or retained fork lease;
a valid application launches immediately afterward. The actual Finder double-click
gate opens QuickDraw Test from a host volume, quits it and relaunches from its
source folder. Separate existing native/classic process and graphical application
switching gates pass. Host format tests cover multi-chunk payloads/maps, empty
resources, malformed ranges, budgets and incomplete delivery. Optimized image,
workspace/boundary checks and generated API documentation pass.

This closes the provider-backed native application loading slice, not target 1 as
a whole: associated documents and the full mutation/transfer/application-drop
contracts remain required. The immutable image-cache budget and supported RV64
relocations remain the existing platform limits; arbitrary live code replacement
and general legacy executable translation are not established by these tests.


### Streaming transfer substrate (current unverified implementation)

Native HFS-B and provider-backed fork writers now have a size-distinct native
service and RAII SDK owner. Both forks stream in bounded chunks; staged data can
be read back before publication. HFS-B publishes fork ownership and metadata
atomically, preserves replacement CNIDs, and reclaims old payload incrementally.
Exclusive boot recovery uses the same bounded reclamation. FileStreams uses the
shared generation facility, with separate application ownership and exit cleanup.
The provider path retains owned continuations across waits, confirms prospective
metadata and actual publication receipts, and drains failed/cancelled operations.
Host wire/HDIR and FSYS contracts are now v3 and require rebuilding all producers.
These are transfer prerequisites, not completion of recursive copy/move or any
of the six acceptance targets. No behavioral or guest verification has run for
this streaming implementation.

### Managed copy presentation and group moves (current unverified implementation)

Finder's resource-based copy-window controller uses the original 7.6.1 fvew8751
bounds, font policy and STR#8750 wording. It owns Static Text, Progress Bar and
Stop controls; filename fitting runs only when the current filename changes.
The progress CDEF computes exact fill damage and skips subpixel changes. The
boot panel uses that same loaded managed control; its separate renderer has
been removed. Original live colours/pattern and interactions remain a deferred
pixel-comparison gate. The controller is connected to stepped folder-to-folder
catalog moves; recursive fork-copy integration remains open.

FileGroups adds shared generation-tagged resident ownership and a stepped
IconMoveGroup SDK guard. The native block path uses its existing atomic catalog
move; provider continuations use Begin/Step/AbortIconMoves, carry owned rows,
preserve committed outcomes and quarantine unacknowledged cleanup. Exit/drop
cleanup remains resident-owned. Provider reply progress is now typed and checked.
Finder's same-volume move admission/receive path now uses that provider group
route. It retains an owned payload, an allocation-free file-reference future
and the storage guard across ordinary event-loop turns. There is no nested
event consumer. Automatic icon-position saves likewise step between normal
events, update only their submitted cached identities, and schedule no repaint
for persistence alone. The old synchronous convenience API remains block-only;
it is no longer used by Finder's drag/automatic-position paths.

Escape/Stop during receive is a cancellation request, not a terminal result.
The Drag Manager releases physical input after mouse-up, retains the receiver
through acknowledged storage cleanup and reports a racing commit truthfully.
Closing a receiver window retains its cleanup capability; a dead receiver uses
resident owner-exit cleanup. CancelRequested uses native OsEvent prefix0x0e,
distinct from process-file0x0c and geometry0x0d. Group moves now capture affected
directory modification dates and restore them after row undo, with volume dates
last. Parent host snapshots refresh after our own namespace changes. Busy folder
state saves defer; a closed window retains a bounded pending save.

These features still await their combined behavioral verification. The user’s
subsequent build/boot failures authorized targeted optimized builds, private
boot diagnostics and focused menu/control/clipping checks; those repairs do not
establish completion of the transfer implementation. The implementation-first
instruction still defers the broad suite and API-document generation until the
remaining feature code is complete. All six acceptance targets remain
open. Retain the original-runtime comparison and unhappy-path matrix as final
acceptance, including whole-batch cancellation after per-file publication.

The original progress drawing routine is now traced (Finder CODE174:0x36e),
including its64-bit copy counters, growing frame and colour/mono paths. Native
ABI11 carries full-width progress counters to avoid intermediate percentage
rounding; all manifests and fixture expectations have been updated together.
Format1 code-resource envelopes remain unchanged. The shared progress presentation now uses the requested fixed black frame,
pale-blue remaining area and dark-grey completed prefix; zero progress has no
minimum marker. It does not present two moving ranges. Only the changed fill
strip is damaged, including return to Preparing. Original running-copy scheduling
and interaction comparisons remain deferred.


### Verified startup repair, 2026-10-22

The reported allocation failure was stack corruption: menu parsing overwrote
Finder's allocator free-list metadata, and window visibility clipping used a
32 KiB quadratic scratch array. Parse allocated menu state in place; sweep
clipped rectangle bands with linear bounded scratch instead. The resident fix
and an immediate fatal-allocation launcher diagnostic are committed/pushed as
982ecb5. These fixes do not increase the System heap or application stack.

Driver manifests and both filesystem fixtures now declare the current HDIR/FSYS
v3 contracts in the working implementation; stale v2 declarations caused the
observed StuffIt BAD_ABI and Host Filesystem ACCESS_DENIED rejections.
An optimized private guest reaches Finder's desktop with all three drivers
loaded and a real host-directory export mounted. Focused SDK tests cover five
menu cases and four managed-control/native-capture cases; thirteen Window
Manager tests include exact occlusion at the 32-window limit. Captured native
boot stages pass Rust pixel assertions for the requested progress presentation.
The complete six-target workflow suite remains deferred and all six boxes stay
open. Source/capture logs are under results/system7/boot-repair outside Git.

### Identity and document-declaration continuation (not yet verified)

Plain writable host objects now persist a qualified incarnation/catalog-ID
annotation before catalog exposure, without saving user metadata or rewriting
forks. Private provisional bindings can adopt a cooperating writer's committed
identity; durable/exposed bindings cannot silently change. Published host write
cleanup must reassert parent-directory durability before retiring its receipt.
Deferred component tests cover reboot, host rename, path deletion/recreation,
and first-bind persistence failure/retry. Container-copy adoption/retirement,
namespace deletion and full transfer integration remain open.

The shared formats crate now decodes/encodes classic Translation Manager open
resources and mac-build has the resources.json open_types payload. Apple
develop18 (June1994), printed p87, confirms the signature/zero/count/FourCC
layout and that opening support is separate from supplied document icons.
Application admission, queued document delivery and Finder target integration
remain required; declaration encoding alone closes none of the six targets.


## Implementation continuation: identity, removals and nested sources (unverified)

Guest and Linux fork tooling now distinguish embedded source-container IDs from
qualified physical-object bindings. Newly created files/folders persist bindings
before returning them; imported/copied containers receive distinct catalog IDs.
Identity-only annotations have revision zero and an empty patch, so adoption does
not invent a metadata edit. Deliberate replacement transfers the logical ID and
retires the old physical binding. Native directory markers are projected through
the directory incarnation; directory edits use small annotations, with physical
names remaining authoritative. Plain files remain ordinary host bytes.

The writable driver now implements owned removal admission/finish/abort over
host quarantine/commit receipts. File and empty-directory removal is routed
through the public SDK and resident write ownership; nonempty directories return
the typed DirectoryNotEmpty cause. Retry retains actual publication state,
acknowledged cancellation refreshes restored source revisions, and parent logical
dates are preserved. Full copied properties/comments publish atomically through
the SDK on HFS-B and host providers, including explicit historical dates and
backup epoch zero. Canonical guest cases cover both paths and failed edits.

StuffIt source changes multiplex up to 32 independent requests instead of
serializing a pending mount against its own backing read. Each open decoder
remains exclusive; pending mount capacity is reserved before lower I/O. The
resident mount admission now consults the generic SERIALIZED offer flag rather
than unconditionally rejecting same-provider sources. New reproducible stored/RLE
wrappers exercise archive-in-archive mounts, backward reads, source leases and
ordered teardown through the public SDK. These assertions are written, not run.

No new compile/test run, commit or push has been made for this continuation.
All six acceptance targets remain open. The shared recursive transfer engine,
whole-batch rollback/source-quarantine integration, Finder copy/cross-volume
wiring, resource-based document delivery/application targets and final acceptance
matrix remain work; these primitives do not establish a finished driver contract.
Host removal receipts now also persist two checksummed phase slots before
namespace mutation/irreversible commit. Encountering private leftovers triggers
identity-checked recovery; normal browsing adds no recovery scan. New deferred
cases cover prepared/quarantined/committing/removed phases, a torn slot, live
journal locks, collisions and subsequent retry. Actual process-kill/guest
verification remains part of the combined acceptance pass.


## Transfer coordinator construction (in progress, not wired/verified)

The pending source now separates foreground parking from owned operation starts
for metadata, fork streams, namespace groups, paths and reads. The background
adapter can reuse those continuations without parking Finder's event loop and
without retaining app buffers or a manager guard. This is construction for the
resident recursive transfer coordinator, not a second filesystem implementation.

The size-distinct FileTransferCall, shared FileTransfers generation namespace
and thin SDK RAII owner/progress/error vocabulary are added. The resident owner,
planner/copy/verify/quarantine/publication/rollback driver, ABI pointer dispatch,
idle/app-exit lifecycle and Finder wiring still need implementation. The new
public transfer entry point is not yet functional. Final delivery must not ship
an unimplemented entry point or close any acceptance target on these pieces.

The coordinator will stage/verify both forks under an invisible destination
transaction folder, keep a rollback ledger through root publication, quarantine
move sources before the irreversible barrier, and reclaim sources/temporary
folders in bounded steps. Apply copied folder metadata in postorder. Mask lock
and name-lock bits on provisional copies until commit so rollback can remove
its own objects. Same-volume moves retain the existing native/provider group
fast path. Global Trash resolves to each source volume's own Trash and always
moves. Pending I/O and progress rendering must remain independent of input.

Before the final combined build, audit/bump the host/HDIR/FSYS required-contract
versions together so old generated companions/packages are rejected clearly;
new mandatory removal capabilities must not accidentally accept an old writable
provider that advertises the earlier contract. Rebuild all native artifacts.


## Resident transfer and Finder wiring source pass (unverified)

The pending continuation now wires the FileTransferCall boundary into the
resident shared handle table, scheduler pump and app-exit abandonment. The
coordinator owns bounded planning, two-fork copy/readback verification, hidden
staging, source quarantine, grouped publication and reverse-order rollback.
Cancellation and the irreversible commit barrier share one resident guard.
Temporary guard failures retain the boxed state; cleanup failures retain the
ledger and leases, with bounded retries and distinct primary/cleanup statuses.
Private directories receive invisible Finder metadata in their creation commit.
Rollback restores affected parent and volume dates through a private primitive.

Finder's receiver now uses the transfer SDK for copy/cross-volume move and
per-source-volume Trash. Its managed copy window follows progress through
coalesced native notifications rather than a hot idle loop; Stop/Escape retain
the drag payload until an actual terminal receipt. Readonly folder rearrangement
remains presentation-only. Watch notices for leased volumes coalesce until the
transfer settles, avoiding provisional-layout churn. The existing provider
MoveToTrash/PutAway calls have a bridge onto the same owner and trap replay
wake path. No classic 68k adapter was changed.

The new canonical transfer_cases module contains deferred public SDK assertions
for recursive native-to-host copies and host-to-native moves, both forks,
properties/comments, same-volume identity, forced-copy naming, readonly and
cycle/collision refusals, cancellation/date restoration/retry and stale/foreign
transfer IDs. These are source assertions only, not passing acceptance evidence.

Still unfinished: provider Trash usage and recursive Empty Trash; complete
source/read protection and recovery UI; application document delivery/type
contracts and application icon targets; archive-to-writable transfer and the
full fault/disconnect/RAII/Finder matrix; combined build/native/visual/performance
verification; contract-version regeneration and docs/api generation. Review of
the resource-editor/extension architecture remains after all six acceptance targets.
No build/test, commit or push was run for this continuation.

## Verified Text Edit / JSON preferences preview, 2026-10-22

Text Edit replaces the TextEdit Exerciser with owned plain-text document windows,
System Open/Save As/Save Changes dialogs, CR/LF/CRLF preservation, font and size
menus, Monaco12 defaults and atomic versioned font preferences. ResEdit's user
visible/installed name is Res Edit. Preference documents use native JSON/TEDT
metadata and extensionless names: File Types and Text Edit Preferences. Editing
an existing JSON document retains its type. Native application declarations accept
TEXT and JSON. The original reference app and earlier System RISC artifacts need
no compatibility path.

The real Finder workflow exposed a native File record-size collision: data saves
and transfer calls were both464 bytes and native dispatch routed saves to the
transfer family. ABI11 gives transfers472 bytes and compile-time checks require
all File request families to remain size-distinct. Every native producer and
fixture is rebuilt together. Immediate edit+Save now stops keyboard batching at
raw Command-key boundaries so the preceding edit publishes its menu policy
before the queued shortcut resolves.

Standard/application menu drawing and hover borrow resident boxed records rather
than copying the25KiB ABI state onto nested application stacks. Optimized present
stack reservation fell from52272 to2640 bytes; ordinary menu hover from25856 to80.
No heap or thread stack budget was increased. Allocation-failure diagnostics now
include allocator headroom. These changes accompany the preview checks; this is
not a completed general stack isolation mechanism.

Verification: optimized image build and HFS-B fork/metadata/blessing checks;
18 focused TextEdit/menu/codec host tests,21 ABI/format unit tests; canonical
physical Finder JSON open/edit/immediate-save/type-preservation/font-persistence
test; real MDEF tracking/keyboard/cancellation/original-pixel gate. Transport
checks preserve the base installation and remove their private disks/ROMs.
The JSON test is included in cargo xtask check's private guest sequence. The
complete six-target driver/transfer acceptance matrix remains open, and this
preview does not authorize committing the unverified larger implementation batch.
