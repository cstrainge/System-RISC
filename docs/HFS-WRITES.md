# Durable File Manager mutations

The resident File Manager now supports creating folders, discovering/creating a
per-volume Trash directory, moving items to Trash, Put Away, recursive Empty
Trash, and counting Trash items and allocated disk bytes. These operations use
real classic HFS catalog records, directory/file threads, both fork extents, the
allocation bitmap, and primary/alternate MDBs. The SDK exposes them through
`System::files()`; applications do not parse or rewrite disk structures.

ABI 5 passes optional sector-write and flush callbacks from the boot ROM. The ROM
negotiates virtio block OUT and FLUSH. A device without durable flush support is
exposed as read-only. Physical read-only devices and either on-disk HFS lock bit
also prevent mutations. Dirty/unmounted-bit-clear volumes remain readable but
their mutations return `NOT_SUPPORTED`.

## Persistence and failure contract

Before the first write, an operation reads and validates its metadata, computes
the replacement trees, checks capacity and fork extents, and captures every
changed sector's original bytes. It then marks both MDBs dirty and flushes,
writes catalog/overflow nodes and flushes, releases allocation bits and flushes,
then commits both MDBs and flushes. File payload sectors are never overwritten.
Moving to Trash retains the same CNID and both forks; Empty Trash frees their
physical allocation blocks only after removing the catalog/overflow references.

A reported write/flush failure triggers restoration of every attempted sector,
including a potentially partially written sector, followed by a flush. Successful
rollback returns `IO_ERROR`. Failed rollback returns `ROLLBACK_FAILED` and poisons
the mounted volume against further operations. A virtio timeout or invalid
completion poisons the queue, since reusing its descriptors could race uncertain
DMA completion. Prepared catalog metadata is returned without additional disk
reads after the commit.

This is rollback for reported I/O errors, **not power-loss atomicity**. There is no
on-disk journal. An interrupted transaction can leave a dirty volume requiring
repair; the implementation refuses to mutate that dirty volume on a later boot.

## Bounded profile

The current writer rebuilds trees inside their existing reserved forks. It does
not grow catalog/overflow forks or provide arbitrary file data writes. Nodes are
512 bytes; the supported header record layout is the classic 14/120/248/504
layout. Reserved forks may contain up to 65,536 nodes, with up to 64 contiguous map
nodes after the header. Live records and rebuilt active nodes each have a 512 KiB
bound, with at most 8,192 records per tree. A transaction journals at most 2,048
changed sectors, approximately 2 MiB of old/new sector storage. Large allocations
use fallible reservation. The existing reader's small name/extent allocations
retain its normal allocator behavior. Capacity failures occur before disk
writes and return `DISK_FULL` or `NO_MEMORY`.

This supports both generated 8 MiB fixtures and the default 500 MiB disk, whose
catalog and overflow forks reserve 4,087,808 bytes each despite a small live
catalog. Only the active tree and its map nodes are staged, not the entire
reserved fork. The real guest passed with the existing 8 MiB System heap.

## Original behavior and explicit differences

Original System 7.6.1 durable reference captures confirm:

- Each volume's root `Trash` directory has Finder flags 0x1000, not the invisible
  flag 0x4000. It is created only on a volume that needs it.
- Concatenated FInfo/FXInfo or DInfo/DXInfo bytes 28..32 hold the original parent
  CNID. Put Away restores that parent and retains this field afterward.
- Empty Trash recursively removes children and preserves the Trash directory.

The writer preserves these behaviors. It rejects root, the blessed System
Folder, its System/Finder files, open items and their ancestors, and locked files.
The current conflict policy returns `ALREADY_EXISTS` without changing either
item. Original Finder instead renames an older colliding Trash item to
`Duplicate copy`; that application naming policy is deferred. Option-overriding
locked items and original replace/error-dialog policy are also deferred.

Original evidence is under `results/finder-menu-trash-reference`, particularly
`19-after-restart`, `28-before-delete-durable`, and `34-after-empty-durable`.
Hashes and original behavior notes are recorded in
`provenance/finder-menus-trash-reference.json` and `analysis/finder-menus-trash.md`.

## Canonical public API tests

All acceptance assertions are in the aggregate crate's tests:

```text
cargo test -p mac-toolbox --test file_mutations
cargo xtask image
cargo test -p mac-toolbox --test file_writes_guest -- --ignored
```

The host tests invoke actual native dispatch and the resident File Manager over
binary HFS files, with independent hfsutils remount checks. They cover both
forks, Unicode/MacRoman names, remounts, physical allocation recovery, protections,
conflicts, software volume locks, dirty volumes, rollback after partial writes
and bitmap flush failure, failed rollback poisoning, and Pascal/MacRoman
`NEW_FOLDER` conversion through the same durable service.

The guest fixture shares its public SDK assertions with a host remount test. It
boots a private writable 500 MiB disk three times through the real ROM, System,
ECALL gate and virtio block driver. The stages verify persistent create/move,
both fork payloads, restore, recursive deletion, exact freed allocation bytes,
and removal after another reboot. Python only prepares and boots the clone;
Rust owns every acceptance decision. The base OS artifacts and original
reference disks are not changed. Results are recorded in
`results/system7/filesystem-write-checks.json`.
