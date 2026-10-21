# Native filesystem class (FSYS v1)

`mac-filesystem-interface` is the safe, allocation-free byte contract between
resident File Manager and independently loaded filesystem providers. It is a
class contract over the generic module broker, not another module ABI. Source
capabilities distinguish host exports from leased backing files; the registry
need not know either filesystem's catalog or compression rules. A directory is
never presented as fabricated sectors.

Resident System supplies generation-checked volume, open and write tokens. The
provider binds them to its owned state, validates the binding on every call,
and retains persistent CNIDs independently. System additionally checks app
ownership. No application pointers, Rust struct layout or allocator ownership
cross the module boundary. Every request is owned while pending; callbacks make
bounded progress, and source/provider leases survive cancellation until ACK.

## Bytes and operations

All integers are little endian. Exact length, schema, reserved fields, bounds,
UTF-8 and catalog structure are checked before use. Unsupported old schemas
require rebuilding producers/consumers; there is no second decoder.

Every request has a 64-byte header followed by at most 64 KiB of payload:

| Offset | Field |
| --- | --- |
| 0, 2 | u16 schema 1, u16 header size 64 |
| 4, 8, 12, 16, 20 | u32 volume, node, token, parent, flags |
| 24, 32, 40, 48 | u64 offset, length, auxiliary, when |
| 56, 60 | u32 payload length, reserved zero |

Operation is the broker operation number. Unused arguments must be zero:

| Operation | Inputs | Reply |
| --- | --- | --- |
| Exports (1) | none | u32 export count |
| Mount (2) | resident volume token; auxiliary = source/export selector | Mount descriptor |
| Unmount (3) | volume | empty; busy if open forks/transactions |
| Catalog (4) | volume, CNID in node; optional INCLUDE_CHILD_COUNT flag | Metadata |
| Scan (5) | volume, parent CNID, child index in offset | u32 pending=0/entry=1/done=2, then Metadata for entry |
| Lookup (6) | volume, parent, UTF-8 name payload; optional INCLUDE_CHILD_COUNT flag | Metadata |
| Open (7) | volume, node, resident open token | u64 data length, u64 resource length |
| Read (8) | volume, token, fork flag 0/1, offset, capacity in length | verified fork bytes, at most 64 KiB |
| Close (9) | volume, token | empty |
| BeginCreate (10) | volume, parent, resident write token, data length, resource length in auxiliary, timestamp in when, name payload | reserved u32 CNID |
| BeginReplace (11) | volume, node, write token, fork lengths and when | empty |
| WriteChunk (12) | volume, write token, fork flag, next chunk bytes | empty |
| FinishWrite (13) | volume, write token | published u32 CNID |
| AbortWrite (14) | volume, write token | u32 publication-observed boolean |
| Refresh (15) | volume | empty; existing read leases remain bound to their original source |

Rename/Move/MakeDirectory/Remove/SetMetadata (16–20) are allocated operation codes;
the host provider currently rejects them as unsupported. Their payload contracts
must be specified and tested with their implementation. BeginCreate currently
creates fresh metadata; metadata-preserving copy needs the subsequent metadata
contract. This checkpoint is not full File Manager mutation coverage.

Metadata uses a 112-byte header followed by its UTF-8 name and comment:

| Offset | Field |
| --- | --- |
| 0, 2 | u16 schema 1, header size 112 |
| 4, 8, 12 | u32 CNID, parent CNID, directory boolean |
| 16, 24 | u64 data and resource fork lengths |
| 32, 40, 48 | u64 created, modified, backup microseconds since 1904 |
| 56, 60 | u32 backup-present boolean, child count |
| 64, 66, 68, 70 | u16 attributes, name bytes, comment bytes, reserved zero |
| 72 | 32 bytes Finder info |
| 104 | u64 metadata revision |

An absent backup date requires its value to be zero. Names/comments each occupy
at most 255 UTF-8 bytes; filesystem-specific storage normalization stays with the
provider. Directory fork lengths must be zero. An unscanned directory's child
count is explicitly `u32::MAX`. Catalog/Lookup flag bit 0 (`INCLUDE_CHILD_COUNT`)
requests an exact directory child count; all other flag bits are rejected.
Without it, metadata queries do not enumerate the target directory just for a
count. A cached exact count may still be returned. Root CNID
is 2, directory=true, parent=0. Other entries have a parent CNID of at least 2.

A Mount reply has a 48-byte header then root Metadata: schema/size at 0/2,
u32 capabilities at 4, filesystem FourCC at 8, whole format version at 12,
capacity-present boolean at 16, reserved zero at 20, u64 total/free at 24/32,
and reserved zero at 40. READ=1 and WRITE=2 are per-mount capabilities; absence
of capacity requires zero fields and means unknown, not a zero-size filesystem.
The format version identifies the filesystem/container contract, separately from
this class's version. HOST currently reports RSFORK01 version 1.

## Writability and cancellation

Every mounted filesystem uses the same rules: incoming copies/moves require a
writable destination; moving, deleting or trashing also requires a writable
source. Copying from a read-only source is allowed. Enforce capability checks in
resident File Manager as well as provider operations, and revalidate after drag
negotiation before making any changes. Do not infer permissions from `.sit`, a
host path, or keyboard modifiers; never silently turn a rejected move into copy.
Cleanup/close/abort must remain possible for a transaction whose mount becomes
unavailable or read-only.

Read-only status does not prevent [file/directory watch registration](FILE-NOTIFICATIONS.md).
Providers without content-change reporting may keep valid subscriptions quiet;
resident lifecycle notifications and object/owner validation still apply.

Cancellation is a request, not a claim that an acknowledged mutation was undone.
HDIR ticket state and request buffers survive abandoned future polls; draining
preserves actual host replies, including committed writes and acquired leases.
The Finder batch transfer layer must roll back its own outputs/reversible moves
when Escape/Cancel cancels the entire operation. No source deletion precedes
successful destination copy and verification.

The host filesystem callback engine owns its client/future and START bytes until
terminal delivery. Each callback performs at most one dependency invocation,
even when the dependency completes synchronously. A smaller POLL buffer retains
the completed result. Cancellation drains actual lower acknowledgements before
closing leases or aborting stages; repeated CANCEL cannot cancel that cleanup.
An unpublished cancellation reports `OPERATION_CANCELLED` (-17), exposed as
`Error::Cancelled` by the native SDK. If FinishWrite observed a commit, it returns
the actual CNID after cleanup rather than claiming that cancellation undid it.
Reply storage is reserved before publishing/opening objects. Deferred orphan
leases stay in the provider's ledger and are retried before the next START;
unrelated cleanup failure cannot erase an already successful operation result.
Quiescing blocks new work but permits Close, AbortWrite and Unmount to drain it.

## Verification scope

The host provider has real-host component tests through the class's owned-byte
codec for mount/catalog, native metadata, both-fork read/create, read-only refusal
and invalid requests before side effects. Additional adapter tests cover bounded
polling, admission contention, live tickets, cancellation, actual publication
outcomes and malformed correlation. Strict host/RV64 library checks pass.

Callback-engine tests use real Linux host exports with fresh poisoned buffers on
every invocation. They cover both-fork round trips, synchronous dependency
budgets, read-only refusal before I/O, malformed START capacity, foreign/stale
request tickets, retained replies, quiesce/cleanup, and cancellation before and
after a real host commit. Resumable scan tests also drop a scan after ListNext
consumed a child and inject metadata failures: retry must not skip that child.

The `host_filesystem_module` private SDK gate also boots the actual release
Host Transport and Host Filesystem packages, relocates both into the resident
module arena, resolves their exact dependencies, and invokes FSYS Exports from
a separately loaded INIT probe. Its request passes through HDIR/virtio-console
to the real loopback Linux companion; successful probe initialization verifies
the returned export count. Rust checks dependency order, successful publication,
desktop boot, absence of traps/rejections, and unchanged base media. This gate is
part of `cargo xtask check`. It verifies native callbacks and transport, not
mounted public File Manager APIs or UI responsiveness during I/O.

The same gate has a separate userspace case calling `Files::host_export_count`,
`mount_host_export` and `unmount` through the real ECALL service. File Manager
owns a common generation-tagged mount table for block and provider volumes;
reservations are hidden until validated publication. Repeated mounting returns
the existing live identity. Mount errors drain Unmount before retiring the
reservation; failed cleanup quarantines the binding instead of reusing its ID.
Unmount is acknowledged before retirement. The caller parks outside the Files
I/O handoff, and the resident retains job/result ownership separately from its
waiting thread. Thread exit requests cleanup without dropping an actual result.

Userspace assertions cover writable/read-only mount metadata, unknown capacity,
source classification, invalid export cleanup, duplicate mounting, unmount,
forced slot reuse and stale/foreign volume rejection. Read-only folder creation
and data-write admission fail as ReadOnly. The public snapshot carries
`Option<VolumeCapacity>`, `VolumeSource` and provider FourCC/whole version;
Finder omits unavailable capacity figures and Get Info describes virtual volumes
without fabricating a block-device number. This gate does not yet establish
write-back, exit during pending I/O, disconnect or responsiveness.

Catalog, direct-child enumeration and parent queries also run through the public
SDK in that guest case, including nested folders, Unicode names, end-of-directory
and non-directory errors, empty read-only roots, and stale mount rejection after
slot reuse. Resident read leases pin the mounted generation until the actual
provider result is consumed, including cancellation/exit cleanup; unmount cannot
race a pending read. Decoded metadata is bound to the requested node or parent.
Mount reads only the root record, with an explicitly unknown child count, rather
than enumerating the export. Catalog requests obtain exact directory counts;
direct-child requests advance bounded provider scans and park between steps.
Both fork types now support public owned open/read/seek/tell/close, including
managed Drop. Each provider read transfers at most 64 KiB and may return a short
read; position advances exactly once on delivery. The guest checks independent
marks, deterministic multi-chunk data/resource payloads, EOF/empty reads, seeks,
busy unmount, Drop and forced file-slot reuse with stale/foreign-ID rejection.
Provider handles use the common resident File Manager table. An exiting owner
marks its provider handles for deferred cleanup instead of removing slots while
the driver still owns their capabilities. Pending reads retain tracking through
actual completion; Close acknowledges retirement. Failed cleanup quarantines
the mark, retaining the mount lease. Resident tests cover pending-read exit
transitions. The guest also launches a real second application, opens/reads both
forks, deliberately forgets both wrappers and exits. The first app then waits for
actual process exit and successful unmount after deferred Close acknowledgements.
Exit during pending I/O, disconnect and latency gates remain outstanding.

Public path lookup/output also pass in the native guest: absolute/relative
paths, repeated separators, dot/parent components, root clamping, Unicode folding,
empty paths, trailing-slash errors, missing entries and output-buffer refusal
without partial writes. The resident owns input bytes across waits and binds
replay to the same request; no caller pointer is retained. Ancestry construction
checks cycles, 256 total nodes and a 4096-byte output bound, accumulating names
once and assembling the final path once. Intermediate metadata queries omit
child counts; only a final directory lookup requests its exact count. Real-host
component tests assert no enumeration for metadata-only queries and reject
unknown flags before host I/O.

Incremental public cursor scans, Resource Manager inspection
and application loading from provider forks remain to be connected. This does
not yet make external directories usable in Finder.

Verification builds now share `<selected build>/host` for workspace host tests,
examples, formatter tools and rustdoc. Native images/binaries and fixture targets
retain their explicit private roots. The selected root is propagated through
xtask's Cargo environment and the Python path helper; reference renderers no
longer each compile another copy of the workspace into a separate ad hoc cache.

`cargo xtask build` also builds and packages Host Transport and Host Filesystem
as release DRVR/DABI files in the selected output's `dist` directory. The safe
filesystem engine is wrapped by a narrow native entry with a private 1 MiB heap
and an exact HDIR dependency. The allocation helper is a separate
`mac-driver-alloc` crate so allocation-free modules do not acquire an allocator
requirement through Cargo feature unification. These packages are not installed
in the default disk yet: provider cursor/resource-inspection/application-loading
dispatch, write-back, normal
launcher export configuration and complete public-SDK filesystem acceptance
remain in progress. Private guest mounting works; that is not yet a usable host
directory in Finder or a mounted StuffIt volume, and is not a hard latency
acceptance result.
