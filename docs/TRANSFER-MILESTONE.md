# Complete external-volume workflows

Authorized 2026-10-21: implement targets 1–6, push verified milestones, then
review the root resource-editor and resources.json specifications together with
extension-driver-system-spec.md. Do not mark either mount driver complete while
its required contracts remain missing.

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

## Current findings

- Native application loading still calls the block-only resource reader while
  holding desktop state. It must use owned provider I/O outside resident guards.
- Existing archive acceptance uses an imported classic-v1 container. The actual
  raw host file transfer/aldussuperpaint3.5.sit has a StuffIt-5 banner, and the
  live desktop reported Io (-9). Add actual-format/raw-host coverage; the v1
  gate does not establish this user's mount workflow.
- Finder currently negotiates Move only and refuses cross-volume receives.
  Provider atomic icon moves are deliberately rejected; neither copy nor full
  provider mutation functionality is complete.
- Resource-defined document dispatch and application drag targets are still
  required; the driver-owned archive open path alone is not a document workflow.

## Milestones

- [ ] Real external opens, document delivery and archive-format regression.
- [ ] Complete provider mutation contracts and persistence.
- [ ] Transfer engine and Finder copy/move/cancellation UI.
- [ ] Application drag targets and complete end-to-end acceptance matrix.
- [ ] Review resedit-editors-spec.md, resources-json-spec.md and
      extension-driver-system-spec.md; record their combined follow-on design.

Use private optimized guests, preserve the visible VM, retain reports outside
Git and prune obsolete media at every check boundary. Passing smaller gates
closes only the named milestone, never the whole driver contract.

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
