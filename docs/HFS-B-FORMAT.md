# HFS-B on-disk format, version 1

This is the byte contract implemented by `crates/mac-hfs-b`, shared by Linux
tools, ROM and System. It follows HFS+'s separation of allocation bitmap,
catalog, overflow extents and attributes, with distinct HFS-B codecs. It is not
an HFS+ decoder or a modification to classic HFS. See Apple's
[TN1150](https://developer.apple.com/library/archive/technotes/tn/tn1150.html)
for the structural reference and [HFS-B.md](HFS-B.md) for product requirements.

All integers are unsigned big endian unless stated otherwise. Sectors are 512
bytes; allocation blocks and tree pages are 4096 bytes. A selected volume range
has an inclusive start sector and sector count divisible by eight, at least 128
sectors, with at most `u32::MAX` allocation blocks. Every offset/range is checked
against that range and device geometry. File lengths are `u64`; neither the
format nor the algorithms narrow them to 32 bits. The allocation geometry bounds
actual physical capacity. There are no sparse/unallocated holes inside a fork.

## Format and Unicode are one contract

**HFS-B version 1 implies Unicode 17.0.0.** The header's Unicode field is a
redundant consistency check, never an independent version selector. The complete
disk/normalization/comparison contract has one whole-number version: `1`, `2`, `3`.
There are no major/minor/patch or fractional filesystem versions; `1.2` and
`2.0` are not filesystem version identifiers. The Unicode release's dotted name
describes its pinned source data, not an additional filesystem version axis.
Normalization and comparison tables are checked in and builds need no network access. Their
official source URLs and SHA-256 hashes are recorded in `names/tables.rs`, with
the Unicode license alongside the crate. Regeneration uses
`scripts/generate_hfs_b_unicode.py`, not the host locale or compiler tables.

Stored names and catalog keys are NFC UTF-8, case preserving, one through 255
bytes after normalization. Input/normalization work has an independent 16 KiB
input cap and bounded scalar/reordering budget. Empty, NUL, slash, colon, `.`
and `..` components are invalid. Ordering and equality use
`NFD(DefaultCaseFold(NFD(name)))` from the pinned data, without Turkic tailoring.
The incoming lookup key is prepared once per tree operation; decoded page keys
cache their transient comparison forms. Folded/decomposed search forms are never
serialized in place of NFC display spelling, including internal separators.

Full case folding maps `ß` to `ss` and `ﬁ` to `fi`; these collide. NFC is not
NFKC: circled `①` remains distinct from `1`. References:
[Unicode 17 CaseFolding](https://www.unicode.org/Public/17.0.0/ucd/CaseFolding.txt)
and [UAX #15 revision 57](https://www.unicode.org/reports/tr15/tr15-57.html).

A Unicode-table change requires a new HFS-B format version. Updating a version
field alone is invalid: regenerate every affected name representation, leaf and
internal catalog key, index and dependent filesystem representation using the
new contract. Check normalization, byte limits and newly equivalent/colliding
names before publishing the new volume. Preserve file identities, forks and
metadata when a future explicit conversion facility performs that rebuild.
Host fork containers and other derived file representations must use the same
new contract when their filesystem version changes. Such a conversion utility
is not implemented here. During unreleased development regenerate all native
media; readers reject unsupported versions with a recreate-volume diagnostic.
Classic HFS remains an independent unchanged external format.

## Volume headers

The primary 512-byte header is at relative sector 2. An identical alternate is
at relative sector `sector_count - 2`. Allocation block zero and the final block
are reserved. Mount requires equal, valid, clean copies; it does not silently
choose an older header. This is corruption detection, not crash recovery.

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 2 | Signature `HB` |
| 2 | 2 | Format version, exactly 1 |
| 4 | 4 | Allocation/page bytes, exactly 4096 |
| 8 | 4 | Implied Unicode version, exactly `0x00110000` |
| 12 | 4 | Flags: dirty = 1, formatting incomplete = 2; all other bits zero |
| 16 | 8 | Volume sector count |
| 24 | 4 | Allocation block count, sector count / 8 |
| 28 | 4 | Free allocation blocks |
| 32 | 4 | Bitmap start block, exactly 1 |
| 36 | 4 | Bitmap blocks, ceil(allocation blocks / 32768) |
| 40 | 12 | Catalog, extent-overflow and attributes tree root block numbers |
| 52 | 4 | Next persistent catalog ID, initially 16; no wrap or reuse |
| 56 | 4 | Blessed System Folder catalog ID, zero if not blessed |
| 60 | 4 | Next allocation search hint |
| 64 | 8 | Created, microseconds from 1904-01-01 UTC |
| 72 | 8 | Modified, same representation |
| 80 | 8 | Nonzero metadata revision |
| 88 | 16 | Caller-supplied volume UUID |
| 104 | 2 | Volume-name NFC UTF-8 byte count |
| 106 | 22 | Reserved zero |
| 128 | 256 | Name bytes, followed by zero padding |
| 384 | 32 | FinderInfo |
| 416 | 92 | Reserved zero |
| 508 | 4 | CRC-32/ISO-HDLC over bytes 0..508 |

CRC-32 uses reflected polynomial `0xedb88320`, initial/final XOR `0xffffffff`.
Zero timestamps mean the epoch. Optional backup dates have a separate presence
flag. Native conversions preserve microseconds and reject overflow/pre-1904
input; classic projections explicitly discard fractions and check `u32` seconds.

## Allocation bitmap and forks

The bitmap is contiguous beginning at block 1. Bit 7 of its first byte describes
block zero; one means allocated. Header, bitmap, tree and payload blocks are all
accounted for. Padding bits beyond the volume block count are set. Allocation
scans from the persisted hint and stages only changed bitmap pages.

A fork descriptor is 80 bytes: logical byte length (`u64`), allocated block count
(`u64`), then eight `(start: u32, count: u32)` inline extents. Logical order is
the descriptor order, followed by overflow records. Empty inline slots are
trailing `(0,0)` pairs. Runs are nonempty, within the data region and cannot
overlap each other or any other live metadata/payload allocation. Physical bytes
are allocated-block-count times 4096. New/exposed bytes are zero-filled.

## B+ tree pages

Each tree is rooted at its header block. Depth is bounded to 16 levels. Pages
grow by splitting and updating the affected ancestor path. Deletion frees empty
children and collapses a single-child root; it does not rebuild other pages.
Underfull nonempty nodes are permitted. No linked-leaf chain is required:
bookmarked scans seek through the validated tree.

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 4 | Magic `HBTN` |
| 4 | 1 | Tree kind: catalog 0, overflow 1, attributes 2 |
| 5 | 1 | Level, zero for leaf |
| 6 | 2 | Record count |
| 8 | 4 | Reserved zero |
| 12 | 4 | This page's allocation block number |
| 16 | 16 | Reserved zero |
| 32 | variable | Consecutive records: key bytes (`u16`), value bytes (`u16`), key, value |
| variable | variable | Zero free space |
| 4090 - 2*i | 2 | Record i start; final entry is end of used records |
| 4092 | 4 | CRC-32 over bytes 0..4092 |

Offsets must be contiguous, ordered and outside the offset table. Keys are
nonempty, at most 1536 bytes; values at most 1024. Strict ordering uses the tree's
key contract. Internal values are exactly a child block (`u32`); each key is a
lower bound for that child's range. Child levels decrease exactly by one.
Empty leaves are permitted only as roots after deletion. Every followed block
must be allocated, in range and have the expected kind, identity and checksum.

## Catalog

Persistent file/folder IDs are `u32`. Root is ID 2 with parent 1; ordinary IDs
start at 16, never reuse or wrap. These are durable filesystem identities, not
resident-manager handles. The catalog has two key families in the same tree:

- Object: byte 0, then catalog ID (`u32`). Value is the entry below.
- Name index: byte 1, parent ID (`u32`), NFC name bytes. Value is catalog ID
  (`u32`). Compare the prefix numerically and names by canonical caseless order.
  Every non-root object has exactly one name index; equivalent duplicates fail.

| Entry offset | Bytes | Field |
| --- | --- | --- |
| 0 | 4 | ID |
| 4 | 4 | Parent |
| 8 | 2 | File flags, bit zero locked |
| 10 | 1 | Kind: file 0, folder 1 |
| 11 | 1 | Backup-date present, 0 or 1 |
| 12 | 4 | Direct child count, zero for files |
| 16 | 8 | Created microseconds |
| 24 | 8 | Modified microseconds |
| 32 | 8 | Backup microseconds, zero when absent |
| 40 | 32 | FinderInfo, including type/creator and Finder flags |
| 72 | 80 | Data fork |
| 152 | 80 | Resource fork |
| 232 | 2 | NFC name byte count |
| 234 | variable | Exact name bytes, no padding |

Folders have empty forks. Parent links are acyclic and child counts exact.
Renaming onto a canonical equivalent of the same object preserves ID and stores
the newly requested spelling in NFC. Rename/move to another object's equivalent
name fails before mutation. Root and the blessed folder have protected deletion.

## Overflow extents and attributes

Overflow keys are ID (`u32`), fork kind (data 0/resource 1, `u8`), then starting
logical block (`u64`). Values are one extent `(start: u32, count: u32)`. Records
continue immediately after the eight inline runs, without missing/extra keys.

Attribute keys are ID (`u32`) then kind (`u8`). Kind 0 is a UTF-8 comment of at
most 255 bytes. Kind 1 reserves original Trash location: original parent (`u32`),
name byte count (`u16`), NFC name bytes. Future attribute contracts require the
same rebuild-together update of producers, consumers and validation.

## Write and failure contract

Callers supply bounded metadata-page, extent-list and I/O work budgets. Both
forks support streaming and random-access reads/writes and resize. File size is
independent of those per-operation limits. Large growth streams bounded calls.

Writes allocate fresh payload blocks, write/flush them while unreachable, then
stage the catalog/extent/bitmap changes. Metadata commit durably marks both
headers dirty, writes only changed pages, flushes, then publishes matching clean
headers. Reported write/flush failure attempts bounded undo of changed pages and
the previous headers. Failed undo leaves dirty state and returns RollbackFailed;
the resident mount must become unusable until checked/recreated. A successful
rollback returns an I/O error with the original reachable filesystem intact.
Free, unreachable payload bytes are not part of that rollback promise.

There is no journal or power-loss atomicity claim. Mount rejects dirty,
incomplete or mismatched headers. Full checking separately validates page ranges,
catalog indexes and parent cycles/counts, extent ownership, allocation bitmap,
free counts and blessing; it does not silently repair damage.
