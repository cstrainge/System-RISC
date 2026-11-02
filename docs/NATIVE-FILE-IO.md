# Native file I/O foundation

The resident File Manager has development operations for ordinary HFS-B fork
writes and shared open descriptions. This is a bounded native foundation;
`std::fs`, resident launch-directory/path resolution, resumable open/truncate
admission and process streams are not delivered by this milestone. The existing
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
| WRITE (41) | At most 64 KiB of immutable input. Access rights are checked. Append selects live EOF and writes within the same exclusive mounted-storage operation, including independent open descriptions. A gap is zero-filled through resident steps before the payload commits; the original operation retains cursor/fork admission across those steps. An empty write neither grows a gap nor moves an append cursor. |
| SET_LENGTH (42) | Input length is the requested target; output length is actual length. One request continues to its target through resident growth or shrink steps with at most 64 KiB of length change per step. It retains its original cursor and I/O subject while parked; a failure reports the actual completed prefix and original cause. Large OPEN_OPTIONS truncation remains a separate unfinished admission path. |
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

Resumable WRITE, SET_LENGTH and SYNC, together with native READ/SEEK/TELL,
use canonical FIOP operation objects and FIOB backing registered with the ordinary
owner walk. The record retains the original typed OS thread identity, genuine
I/O subject/account and shared description. Admission copies semantic scalars and
write bytes; no application pointer or Files guard crosses a scheduler wait.
Reads own a bounded result and copy into a fresh caller span on delivery. A
completed operation is acknowledged before consuming its result or modifying
fields used to recognize replay. Closing a public handle does not destroy an
already admitted operation's description. Thread/owner retirement requests
cancellation; an in-flight device must actually return before terminal cleanup.
These cancellation and concurrent-close paths remain runtime-unqualified.

Operations sharing a cursor or mutating the same physical fork retain admission
between steps; unrelated file operations may execute between device steps.
Existing staged save/transfer admissions retain their own conflict rules.
Firmware still masks interrupts for a complete device step. The 64 KiB data/size
bound does not establish a bound on metadata processing or target latency.

Two open-admission gaps remain before std adoption: create/truncate can commit
before late FFRK/description allocation or publication refuses, and that open
path still needs genuine preadmitted publication capacity plus explicit retained
handling of any committed failure. Requests also need allocation-free actual
handle/owner/mode preflight before owned staging, followed by revalidation at
custody publication. The finite observations below do not qualify refusal under
memory pressure or make these paths ready for std OpenOptions.

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

A subsequent continuation artifact ran on the same private native QEMU
configuration through ordinary Finder/APPL loading and real SDK FILE traps:

- System runtime ELF: `2f9bbb83f1d9848884cc384b6bcac049974564e87d69c744992464799312812b`
- System resources: `5060f27f76049ab94fb398b5d1a527db36e65530c69e97c89b39fee754fb1c23`
- ROM: `bf45b1962af8d35c3eb6acf80f2de4df692dce8529fe729df2f05fa7ff399e08`
- Diagnostic runtime ELF: `6e77aa0d732c5fc0904f4cc01eb27aa2cc34f2d5aacc1337b7d0468926bb62e3`

The diagnostic issued one request to grow to 196,627 bytes, one write after a
131,085-byte gap and one request to shrink to five bytes. It observed their full
results, zero-filled bytes, unchanged resize cursor and preserved resource fork,
without application-side continuation loops. Empty gap writes left length
unchanged. The earlier finite file cases were repeated, including ordinary
failures followed by reuse. Actual Process/account removal, live Finder directory
refresh, physical folder-close input and saved bytes after a clean second boot
were observed; both final screens were reviewed. Sources/base images remained
unchanged, both owned guests exited and private generated media was removed.
This qualifies those finite paths on those artifacts, not arbitrary file sizes.

These observations do not establish power-loss durability, arbitrary-size file
behavior, late-open admission refusal, provider clone cleanup, sustained overlap,
memory/disk exhaustion, cancellation, rollback-failure recovery, SMP safety,
latency bounds, other hardware or complete std/application adoption. Such paths
remain unverified. Compilation and formatting are build checks; no host Rust
suite is used as runtime validation.
