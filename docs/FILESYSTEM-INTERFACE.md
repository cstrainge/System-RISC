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
| Catalog (4) | volume, CNID in node | Metadata |
| Scan (5) | volume, parent CNID, child index in offset | u32 pending=0/entry=1/done=2, then Metadata for entry |
| Lookup (6) | volume, parent, UTF-8 name payload | Metadata |
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
count is explicitly `u32::MAX`; Catalog/Lookup obtain the exact count. Root CNID
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

## Verification scope

The host provider has real-host component tests through the class's owned-byte
codec for mount/catalog, native metadata, both-fork read/create, read-only refusal
and invalid requests before side effects. Additional adapter tests cover bounded
polling, admission contention, live tickets, cancellation, actual publication
outcomes and malformed correlation. Strict host/RV64 library checks pass.

The loadable filesystem wrapper, resident dispatch and public-SDK guest mount
acceptance remain in progress. Passing these component tests is not a mounted
host directory or StuffIt volume, and is not a hard latency acceptance result.
