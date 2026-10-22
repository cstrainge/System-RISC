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
