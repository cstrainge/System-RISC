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
