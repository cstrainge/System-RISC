# Native file I/O foundation

The resident File Manager has development operations for ordinary HFS-B fork
writes and shared open descriptions. This is a bounded native foundation;
`std::fs`, resident launch-directory/path resolution, general resumable large-file
operations and process streams are not delivered by this milestone. The existing
transactional Save/replace writer remains a separate API.

FileCall uses the existing FILE trap, ABI version 1 and the whole-world native
layout fingerprint. Rebuild System, ROM, applications, definitions, drivers and
SDK consumers together. WRITE has a separate immutable input span; transport
seals its operation and clears borrowed pointers after return. The resident owns
its bounded staging. Open handles remain canonical generation-checked FFRK
objects charged to their genuine owner.

| Operation | Current development contract |
| --- | --- |
| OPEN_OPTIONS (40) | Volume plus existing CNID and empty name, or parent CNID plus one native UTF-8 leaf name. Select data/resource fork and explicit read/write/append/create/create_new/truncate flags. Unknown bits and invalid combinations refuse before mutation. |
| WRITE (41) | At most 64 KiB of immutable input. Access rights are checked. Append selects live EOF and writes within the same exclusive mounted-storage operation, including independent open descriptions. Gaps larger than 64 KiB currently refuse; general gap continuation is unfinished. |
| SET_LENGTH (42) | Input length is the requested target; output length is actual length. Growth currently advances at most 64 KiB per call. This stepping interface must become a real resident continuation before implementing std's single-call set_len contract. Large truncation remains unqualified. |
| SYNC (43) | Calls actual backing-device flush; SYNC_DATA currently requests the same stronger full flush. Unsupported backends return an error. |
| DUPLICATE (44) | Creates another independently owned handle sharing fork identity, access mode, cursor and underlying lifetime. Temporary operation references do not count as open handles. |

SEEK permits positions past EOF; subsequent native reads return zero at EOF.
SEEK_START uses the signed i64 wire offset, so public Start(u64) clients must
reject values above i64::MAX through checked conversion. Relative/end arithmetic
is checked. TELL and native reads refresh actual HFS-B lengths across independent
descriptions. Data-fork truncation preserves the resource fork.

A local operation moves the actual device/cache/partition state out of the
mounted slot while retaining its canonical incarnation and genuine I/O subject.
Firmware runs on the existing fixed-owner transport with Files and operation
state locks released. Terminal device state and typed result are retained before
Files reacquisition; refusal keeps the original custody for maintenance retry.
Publication preserves cache, poison, scan snapshot and catalog revision state.
Fresh scan snapshots describe the actual committed catalog, rather than turning
an invalidated supported scanner into an Unsupported error. Exact partition
aliases share the immutable native snapshot; unequal overlapping mounted layouts
refuse mutation. This is not authorization for foreign-hart firmware callbacks.

Ordinary writable operations currently support native HFS-B. Classic HFS and
provider operations without the required implementation return explicit errors.
Provider clone token/close ownership has source changes, but no provider-clone
runtime qualification follows from the local HFS-B observations below.

The native Std Files Native diagnostic ran through ordinary Finder/APPL loading,
public SDK calls, real traps, canonical objects and firmware-backed storage in
optimized release/LTO RV64 QEMU, with four configured CPUs, 8 GiB and 800x600
output. The corrected artifact set was:

- System runtime ELF: `4cfa5eb9bfaac3246e6707a49e7152646f793dc259d968e4330f0e8503dfcf31`
- System resources: `5964a68e1b0afbca86be135c2765f2d7bafefdd873ea737037dc7dcaf4425c8d`
- ROM: `bf45b1962af8d35c3eb6acf80f2de4df692dce8529fe729df2f05fa7ff399e08`
- Diagnostic runtime ELF: `1cbc23324cbc40c19fde3e0ebba2e62506ceab376f1acda739fefede778ec7da`

Finite observations covered create_new refusal, shared clone cursor and nonfinal
close, independent-open append and live length, small gap zeroing, truncate/grow,
resource-fork preservation, read-only refusal followed by successful reuse, stale
handle rejection, sync return and saved bytes after a clean second boot. Actual
Process/account removal and subsequent Finder folder close were observed. A
further capture on the unchanged corrected artifact observed the open root
folder acquire the new file without reboot. Sources/base images stayed unchanged;
owned guests were reaped and private generated media removed.

The first artifact exposed a Finder update alert after file mutation because
publication cleared the supported scanner. Its failure evidence was retained;
file assertions alone were not accepted as a successful workflow. The corrected
runs included real Finder command dispatch and visual review. Collection used
existing physical UI observation and input, with no host acknowledgement between
file operations, service replacement or clock warp.

These observations do not establish power-loss durability, large-file behavior,
provider clone cleanup, sustained overlap, memory/disk exhaustion, cancellation,
rollback-failure recovery, SMP safety, latency bounds, other hardware or complete
std/application adoption. Such paths remain unverified; no host Rust suite was
used as OS acceptance.
