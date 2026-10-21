# Resident File Manager

The System owns the block-backed HFS/HFS-B service and all open fork marks.
Filesystem-class drivers own their external formats; File Manager owns their
mounted identities and publishes validated metadata in the same mounted table.
`toolbox-files` contains
borrowed native service calls and small typed handles; applications neither parse
the disk nor embed a catalog. Finder can therefore enumerate the actual disks
attached to QEMU, including disks other than the startup volume.

Firmware retains every successfully opened block device for the whole System
lifetime. BootInfo supplies stable disk ordinals, device information and checked
512-byte sector read/write and flush callbacks. `BootStorage::from_boot_info` is the narrow unsafe
boundary: its context/callbacks must remain live and storage calls serialized.
The kernel initializes the safe `Manager` once and serializes requests separately
from its drawing/runtime manager.

`Manager::new` recognizes native HFS-B, bare classic HFS and every `Apple_HFS` entry in
a 512-byte Apple Partition Map. APM logical data offsets/counts are checked and
applied. Each mounted volume has a generation-tagged nonzero ID for its lifetime.
Discovery is bounded to 32 devices and 128 mounted volumes; exceeding those capacities fails
explicitly. Non-HFS devices and malformed volumes are skipped and counted in
`rejected_volumes`. This count includes rejected devices/partition maps, not just
partitions. Provider mounts can be unmounted after acknowledgement; block-device
hotplug/unmount remains outside the current slice. Host export mount/count/
unmount now has private public-SDK guest acceptance. Provider catalog/fork and
write dispatch is still in progress; see [the filesystem class contract](../../../../docs/FILESYSTEM-INTERFACE.md).

Provider requests own their input/results, select the exact generation-checked
interface incarnation, and park the caller outside the synchronous firmware I/O
handoff. A separate resident-owned job retains actual publication/cleanup state
if its waiting app thread exits. Unavailable cleanup quarantines a mount rather
than recycling its identity. Provider volume snapshots carry an explicit unknown
capacity and non-block source; applications receive Rust enums/options rather
than internal sentinels.

The native API provides:

- Mounted-volume enumeration, startup identity, free/allocation bytes, original
  MDB attributes/dates and all 32 MDB FinderInfo bytes. Driver icon type/ID
  supplied by firmware is retained on every mounted partition of that device.
- Catalog lookup by persistent CNID, direct-child enumeration, parent lookup and
  UTF8 path resolution. File/folder metadata includes both fork lengths, type and
  creator, record flags, valence, HFS dates, and all 32 original FinderInfo bytes.
- Independent open marks for data and resource forks, bounded partial reads,
  position/length queries, checked start/current/end seeks, and explicit close.
  Seeking outside `[0, fork_length]` is rejected and leaves the mark unchanged.
  Reading at EOF returns zero and leaves the output tail untouched.

A zero directory CNID means root CNID 2. Directory enumeration is zero based in
native HFS catalog order. Names are converted losslessly between Mac Roman and
UTF8. Lookup uses the classic HFS `FastRelString` equality classes, including its
space/nonbreaking-space equivalence and historical treatment of later Mac Roman
uppercase additions, rather than Unicode lowercase rules. Apple's reference is
[`gCompareTable` and `FastRelString`](https://github.com/apple-oss-distributions/hfs/blob/main/lib_fsck_hfs/dfalib/SKeyCompare.c)
and its [comparison table](https://github.com/apple-oss-distributions/hfs/blob/main/core/UCStringCompareData.h).

Native paths use `/`, `.` and `..`, with a leading slash starting at the selected
volume's root. They do not use classic colon syntax. Catalog enumeration and CNID
operations still address files whose original names contain a literal slash;
`PATH` returns `NOT_SUPPORTED` for such an unrepresentable path. Paths are at most
4096 UTF8 bytes and parent traversal is bounded/cycle checked.

There are at most 64 open forks. Generation-tagged handles reject closed/reused
slots. The SDK's `OpenFile` closes explicitly or on drop; closing explicitly lets
the caller observe an error. Metadata and small extent descriptors are retained;
fork contents stream directly into caller buffers. Each operation remounts its
volume using bounded MDB/B-tree buffers rather than retaining a self-referential
disk borrow. This favors a simple safe implementation over lookup speed.

Each open mark also belongs to one trusted application identity. The native trap
captures that identity before switching to the storage thread and passes it to
`dispatch_owned` / `request_owned`; `FileCall` cannot choose an owner. Reading,
seeking, inspecting or closing another application's live handle returns
`ACCESS_DENIED` and preserves the owner's mark and destination buffer. Closing
an application invokes `close_all(owner)`, which releases only its marks without
resetting slot generations. A relaunched application therefore cannot reuse a
stale handle. Owner zero is the separate trusted loader/bootstrap namespace used
by internal `request` calls, rather than an application bypass.

Open forks support reads, not byte writes. Writable volumes additionally support
bounded durable folder creation and Trash/Put Away/Empty Trash catalog mutations
through the firmware write/flush boundary. All applications' open forks protect
their files from these mutations. There are no general fork writes or arbitrary delete services; the additive
Get Info metadata operations are described below.
HFS+ and non-512-byte APMs are unsupported. Existing HFS B-tree/overflow checks
still apply: scans are bounded, normal file/data/resource overflow extents are
supported, but growth of the extents-overflow tree itself beyond its inline
extents remains unsupported. Invalid filesystem structures and firmware read
failures become `NOT_SUPPORTED`; absent objects/handles become `NOT_FOUND`.

## Verification

`python3 scripts/check_system7_files.py` runs the fixture/SDK tests, streams and
compares the generated HFS System fork, optionally checks the available isolated
original reference, and runs host/RV64 Clippy. It writes exact commands, logs,
source hashes and results to `results/system7/filesystem-checks.json`. Use
`--skip-clippy` when a surrounding build already runs those checks.

Fixture tests cover multiple disks and APM partitions, full metadata, Unicode
names, parent/path traversal, both forks, independent marks, cross-sector partial
reads, EOF, rejected seeks, stale handles, capacity, and the safe public
`Files -> Client::file -> FileCall -> Manager` path. The transport test also proves
that caller-supplied raw pointer fields are replaced and cleared on success and
error. HFS tests retain the fragmented overflow-fork and malformed-tree fixtures.
Canonical `mac-toolbox/tests/file_ownership.rs` additionally exercises trusted
application context selection through the real public gate: foreign-handle
denial, independent marks, paired legacy-gate enforcement, owner-specific exit
cleanup and rejection of old handles after slot reuse.

```sh
cargo test --manifest-path System7/Cargo.toml -p mac-hfs
cargo test --manifest-path System7/Cargo.toml -p mac-system --lib toolbox::files
cargo clippy --manifest-path System7/Cargo.toml -p mac-hfs -p toolbox-files -p mac-system --lib --tests -- -D warnings
```

An explicitly enabled test streams actual immutable disk images, then repeats
resource-fork reads through the resident callback adapter. Set the optional
expected fork to also require byte-for-byte agreement with an independently
extracted resource fork:

```sh
SYSTEM7_TEST_HFS="$PWD/build/system7/dist/system7.hfs" \
SYSTEM7_TEST_SYSTEM_RSRC="$PWD/build/system7/dist/System.rsrc" \
cargo test --manifest-path System7/Cargo.toml -p mac-system --lib \
  toolbox::files::tests::actual_disk_catalog_and_both_forks -- --ignored
```

This was also run against the isolated original System 7.6.1 HFS reference image,
comparing its complete 5,373,687-byte System resource fork with its independently
extracted MacBinary fork. No VM was stopped or disk modified for these checks.

## Get Info metadata services

Additive native FileCall operations19..22 expose exact allocated data/resource
fork lengths, rename, file locking and file stationery. Existing ABI6 request and
catalog layouts remain unchanged. `Files::allocation_info` returns typed physical
byte counts; directory allocation is zero and does not imply a recursive total.
The reader validates physical lengths against logical EOF and allocation-block
geometry, including allocation reserved beyond logical EOF.

`Files::rename` accepts a Rust UTF8 string, converts it losslessly to1..31 MacRoman
bytes and updates the catalog key and any existing file/folder thread. Folder
threads are mandatory; optional file threads remain optional. Case-only renames
retain the same CNID. Fork payloads, extents, creation/modification/backup dates
and unrelated Finder metadata are preserved. The parent modification date and
volume write date advance. Duplicate HFS names, colon/NUL, empty names, dot
components and unrepresentable Unicode fail explicitly. Literal slash remains
addressable by CNID but cannot be returned as a native slash path.

`set_locked` changes only the file catalog lock bit and permits unlocking.
`set_stationery` changes only Finder flag0x0800; Apple's
[TN1150 Finder Info](https://developer.apple.com/library/archive/technotes/tn/tn1150.html#FinderInfo)
identifies that bit as file-only. Both controls reject directories in this native
profile. Repeating an unchanged setting or unchanged name writes no sectors.

These services retain the current conservative mutation policy: root/blessed
System Folder/active System and Finder and open items or their ancestors are
protected. Rename additionally rejects the catalog lock and Finder name-lock;
stationery changes reject catalog-locked files. This policy is explicit and is
not a claim that every original File Manager operation has identical restrictions.
Volume read-only/dirty checks, bounded transaction capacity, rollback and poisoned
volume handling are shared with existing durable mutations. No general fork
writes or volume renaming is implied.

Canonical public SDK tests in `file_mutations.rs` cover independent HFS remounts,
file/folder and case-only/31-byte Unicode renames, both fork payloads and exact
allocation, unrelated metadata preservation, no-op writes, rejected mutations and
partial-write rollback/failed-rollback poisoning for all three mutation services.
The shared `file_write_cases.rs` additionally checks renamed files/folders,
stationery and lock persistence across three actual guest boots when that ignored
RV64 gate is run.

### Incremental enumeration design (not yet implemented)

Existing indexed `child` rescans catalog leaf nodes and must not back a synchronous
whole-volume recursive Get Info call. The next service should expose an opaque,
application-owned cursor containing a leaf node/record position and MDB write
revision. Each operation scans at most a bounded number of leaf records, returns
entries plus continuation, and verifies its revision on resume. Catalog mutation
invalidates the cursor explicitly; callers restart rather than silently combining
snapshots. Retired applications must release resident cursor slots. Per-entry
logical/physical metadata should travel together to avoid separate CNID rescans.
A Finder-owned cancellable walker can then update folder totals between event
waits, with bounded memory and checked ancestry/cycle handling. No API should hold
the File Manager guard across user code or recursively scan the whole disk in one
interrupt-masked ECALL. Existing synchronous metadata mutation transactions still
need a future resumable storage-worker path for strict input-latency guarantees.


## Persistent Desktop DB comments

`Files::comment(volume, cnid, &mut impl fmt::Write)` returns a UTF8 file/folder
comment. `Files::set_comment(volume, cnid, &str, Timestamp)` commits it; empty text
removes only that comment record. Both require a live catalog identity. The public
`COMMENT_MAX_BYTES` limit is255 losslessly encoded MacRoman storage bytes. These
use additive FileCall operations23/24 and leave existing ABI6 layouts unchanged.

The format follows the original captured System7.6.1 Desktop DB: type3, five-byte
key payload containing a big-endian CNID, followed by Pascal MacRoman value and
record padding. The read-only original98,304-byte B-tree and decoded observations
are frozen under `mac-toolbox/tests/data/desktop-comments`; its provenance lists
the original source and the two explicit CNID remaps used only for test transport.
The native writer preserves all opaque unknown leaf records byte-for-byte and
in their existing relative order, and retains header user data while rebuilding
node links, indexes and allocation maps. A full existing data fork returns
`DISK_FULL`; this slice does not silently grow it or alter its resource fork.

A missing database is created as an invisible root `Desktop DB`, typeBTFL,
creatorDMGR, with an empty resource fork and64KiB logical data capacity. It
allocates at most three inline runs, validates bitmap free counts against every
referenced live fork before allocation, and stages new payload, bitmap, catalog
and both MDBs in the same bounded rollback transaction. Reserved allocation beyond
logical EOF is zeroed on creation. Insufficient space, unsupported fragmentation,
malformed databases or conflicting root objects fail before any write. Target
file/folder creation/modification dates, CNID, forks and unrelated metadata stay
unchanged; only database/volume metadata reflects the comment commit. Locked
files and read-only volumes reject changes. Applications commit at edit completion,
not per keystroke.

Canonical tests additionally compare all2,316 unrelated records in the captured
original database after native comment replacement/insertion/deletion, verify
creation/remount/255-byte Unicode/update/delete, check target-date preservation,
and inject partial-write and flush failures across new-file allocation and
existing-database writes. Failed rollback poisons the mounted volume. Comments
also participate in the shared three-boot real guest persistence case. No
Desktop DF support, old FCMT fallback, complete Desktop Manager API or power-loss
journal is claimed.
