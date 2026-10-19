# Resident read-only File Manager

The System owns the HFS parser and all open fork marks. `toolbox-files` contains
borrowed native service calls and small typed handles; applications neither parse
the disk nor embed a catalog. Finder can therefore enumerate the actual disks
attached to QEMU, including disks other than the startup volume.

Firmware retains every successfully opened block device for the whole System
lifetime. Native ABI 4 supplies stable disk ordinals, device information and one
512-byte sector-read callback. `BootStorage::from_boot_info` is the narrow unsafe
boundary: its context/callbacks must remain live and storage calls serialized.
The kernel initializes the safe `Manager` once and serializes requests separately
from its drawing/runtime manager.

`Manager::new` recognizes bare classic HFS volumes and every `Apple_HFS` entry in
a 512-byte Apple Partition Map. APM logical data offsets/counts are checked and
applied. Each mounted volume has a stable nonzero ID for this boot. Discovery is
bounded to 32 devices and 128 HFS volumes; exceeding those capacities fails
explicitly. Non-HFS devices and malformed volumes are skipped and counted in
`rejected_volumes`. This count includes rejected devices/partition maps, not just
partitions. There is no hotplug or unmount operation in this slice.

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

This slice is read only even when firmware reports a writable disk. There are no
write, create, rename, delete, allocation, metadata mutation, or flush services.
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
