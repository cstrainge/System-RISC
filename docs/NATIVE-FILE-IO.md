# Native file I/O foundation

The resident File Manager has development operations for ordinary HFS-B fork
writes and shared open descriptions. The platform std backend connects basic
File/OpenOptions, Read/Write/Seek, buffering, metadata and current-directory APIs
to those actual services. This is a bounded native profile, not complete std
filesystem support. GUI streams deliberately use the null profile described in
RUST-STD.md; real CLAP streams remain future work. Resident path operations,
GUI Documents-directory admission and bounded prepared open/truncate have
connected native implementations. Transactional Save/replace remains separate.

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
| SET_LENGTH (42) | Input length is the requested target; output length is actual length. One request continues to its target through resident growth or shrink steps with at most 64 KiB of length change per step. It retains its original cursor and I/O subject while parked; a failure reports the actual completed prefix and original cause. OPEN_OPTIONS truncation uses the same bounded device-step rule. |
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

Resumable OPEN_OPTIONS, WRITE, SET_LENGTH and SYNC, together with native READ/SEEK/TELL,
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
Queued native requests retain lifetime protection while an already admitted
staged Save or icon group finishes. Volume mutation exclusion starts atomically
with acquisition of the first actual mutating device step and remains through
continuation/result publication. A waiting request must not block the existing
group's next step. No effect is replayed as a retry of transient admission.
Existing staged save/transfer admissions retain their own conflict rules.
Firmware still masks interrupts for a complete device step. The 64 KiB data/size
bound does not establish a bound on metadata processing or target latency.

Native OPEN_OPTIONS prepares the real canonical FFRK publication permit, wire
identity and shared description before create/truncate effects. Actual global
publication credits protect shared completion metadata capacity. Partial truncate
continues with the same resolved catalog identity, permit and operation; a late
publication refusal retains the actual committed outcome and original value
rather than repeating create/truncate. Final owner-stop refusal is still possible.
Backing removal returns its real affine owner for disposal outside Files; cleanup
withholds its original storage ticket while genuine publication preparations
remain. Forced refusal, cancellation and rollback paths still need target evidence.

Handle requests perform allocation-free canonical owner/handle/access-mode
preflight before admitting a Subject or copying write bytes, and repeat it before
operation publication. An admitted continuation keeps its genuine description
when the public handle closes; replay does not require reopening that handle.

GUI application launch admits the user's `Documents` directory on the unique
startup volume after required services are READY and before author execution.
The genuine root alias protects resolution; the validated Documents alias is
published before that root protection retires. There is no executable-parent or
root fallback when Documents is absent or invalid. The prospective owner's actual
authority/account and catalog alias own the admission. Original executable
CatalogInfo and executable protection remain independent. Provider admission
retains the genuine job across scheduler defer; cancellation retains protection
until its actual subject scope closes. Successful admission releases the operation
root while the owner retains its directory alias. Explicit later chdir replaces
that directory normally. Initial/replacement Finder consumers and provider,
cancellation and owner-stop qualification remain separate gates.

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

The preflight correction subsequently ran on an actual private RV64 QEMU guest
with exactly 2 GiB RAM (both launch command and guest BOOT_RAM report checked).
System ELF `74cb0db128c5c1f6028967c2f1dfad453e7442613952181857b4d7ce10b4cd54`,
resources `25d969213970a2946afc4a6b9ef2ab090702959699bd6eb3062d9f30e7287b69`
and diagnostic ELF
`3c52203e4f59735d861e7515097bd14d3fc9704a5945af98fb622c6195300931`
used the unchanged ROM listed above. Full 64 KiB writes through a closed handle
and a read-only handle returned NotFound and AccessDenied, with live file length
unchanged; subsequent resize/write/sync/reopen and the existing finite scenarios
completed. Both actual Process/account exits, Finder refresh and folder closes,
clean-reboot saved bytes and reviewed final screens were observed. Sources/base
stayed fixed; owned guests exited zero and generated private media was removed.
This does not qualify forced allocation failure, foreign-owner overlap, races,
cancellation, global memory pressure or late open-publication failure. Older
8 GiB observations retain their original configuration and are not relabeled.

The prepared-open and ordinary launch-directory consumer subsequently ran on
optimized release/LTO RV64 QEMU with four configured CPUs, exactly 2 GiB RAM
(actual command and guest BOOT_RAM checked) and 800x600 output:

- System ELF: `225fc0a920580c6e5f8a2745f98697f5b124ab15f8015ba72c29209b9e56c3f4`
- System resources: `d3286623e8875e7eb0ad275ab06042e690707633ba6b5db8e4b5bab76e3955ce`
- ROM: `19cc55b172094ec7505d461be6dca90f48f6d87e47cd078c150db2e8771a50e2`

The real APPL diagnostic issued one OPEN_TRUNCATE against a 196,627-byte file.
It observed length zero, refreshed length through another open description,
that description's unchanged cursor, preserved resource-fork bytes and a
subsequent successful write. Earlier finite file/refusal/reuse cases repeated;
a clean second boot read saved SYNC bytes. Both actual Process/account removals,
Finder refresh/command-close/input and reviewed final screens were observed.
Sources/base stayed fixed, both owned guests exited zero and private generated
media was removed. This exercises the connected native ordinary launch-directory
happy path; it does not qualify provider deferral, forced post-effect refusal,
stop/cancel/rollback, arbitrary pressure, public std paths/streams or full adoption.
A preceding host launch was refused before boot because its QMP pathname was too
long. Another capture is excluded because final evidence cleanup failed; its
original logs remain failure evidence. The final repeat uses the same target
artifacts with corrected host socket/receipt paths.

## Process paths and current directories

The native FileCall operations CURRENT_DIRECTORY (46), CHANGE_DIRECTORY (47),
RESOLVE_PATH (48) and OPEN_PATH (49) use the existing FILE trap and genuine
current-owner admission. Opcode 45 is reserved for the separate FindFolder
contract. All affected artifacts and clients must be rebuilt together at ABI 1.

Paths are UTF-8. Absolute paths start `/Volumes/<mounted volume name>/`;
volume-name selection is exact and must be unique, with duplicate matching names
returning AlreadyExists. Relative paths start at the process current directory, initially the user's
Documents directory for GUI applications. Slash-separated components, repeated slash,
`.` and `..` are walked against actual catalog entries; `..` at a volume root
stays at that root. Empty non-query paths and embedded NUL refuse as InvalidInput.
Volume names containing slash or NUL are unrepresentable. Filesystem component
encoding/case behavior remains the actual mounted filesystem's contract; there
is no lossy name substitution. The virtual namespace `/Volumes` itself is not a
filesystem object supplied by this initial native profile.

A request shares the actual live FPIN directory alias into its original DIOP
scope before any scheduler wait. Absolute requests protect their admitted actual
mounted root instead. A copied volume/CNID is metadata, not a substitute lease.
The process may change directories while another operation retains its genuine
snapshot. Directory replacement admits the new alias before removing the old;
refusal leaves the old directory intact. Temporary aliases release only after
that actual operation scope closes, including owner cleanup and cancellation.

The operations reuse FIOP/FIOB admission, real fixed-owner device handoff and
one native catalog/lookup step per maintenance turn. Neither Files nor Record
is held across firmware. Metadata steps do not build a mutation-protection
vector. OPEN_PATH prepares the real FFRK identity/description before traversal,
then resolves the final leaf and performs open/create/truncate within that same
owned continuation; it does not implement a client-side resolve/open race.
TELL also returns actual descriptor catalog metadata for later std File::metadata,
without re-resolving a saved pathname. This initial path profile supports native
HFS-B; provider path requests explicitly return Unsupported.

The corrected implementation ran through ordinary native APPL loading and actual
FILE traps on optimized release/LTO RV64 QEMU with four configured CPUs, exactly
2 GiB (actual command and BOOT_RAM checked), and 800x600 output:

- System ELF: `d49e606c1fbb0c38eb73529703db370ddbbcf088ba425bf5a138390d2d73c55e`
- System resources: `ea23115743f60a3f9efcf22581cb2a9a9af7e5099120e7817dc28bfa542ddc73`
- ROM: `19cc55b172094ec7505d461be6dca90f48f6d87e47cd078c150db2e8771a50e2`

Both boots observed the actual launch parent, relative root traversal, relative
and absolute directory changes, relative create/truncate and absolute reopen with
PATH bytes. Change-to-file, missing-directory, embedded-NUL and create-new-existing
requests refused, followed by successful current-directory reuse preserving the
old directory after failed changes. Actual descriptor metadata identity was
observed. The previous finite file/open/truncate/cursor/resource/refusal cases,
clean-reboot SYNC bytes, original Process/account removal, Finder refresh/close
and following input completed. Sources/base remained fixed, guests exited zero,
private media was removed and both final screens were reviewed.

The first two path captures remain failures: CURRENT_DIRECTORY was refused
because its new trap opcode had not captured genuine caller authority. The fix
extends the existing current_owner capture; it does not relax authority checks.
Interrupted pre-reboot builds are not successful build evidence. The corrected
repeat does not qualify forced concurrent chdir/unmount, cancellation/owner-stop,
provider paths, namespace mutation races, exhaustion, general SMP or std adoption.

## Bounded actual std File and GUI stream observations

The real ordinary-main std APPL diagnostic subsequently ran through Finder,
normal packaging/loading, real traps and native HFS-B on optimized release/LTO
RV64/RVA23 QEMU, four configured CPUs, 800x600 and exactly 2 GiB RAM. Each of two
diagnostic variants ran two boots; actual `-m 2G` and guest `BOOT_RAM` bytes
2147483648 were checked. The matching System/SDK artifacts were:

- System ELF: `e71d3a9e543642e10ea19415e899b1c02a4d1fc93d117bbf7605ac927b38a632`
- System resources: `cf51b82a09001661b662c7b9a9d4868250f3e3af8bdc4baa133d5c11f002047b`
- ROM: `19cc55b172094ec7505d461be6dca90f48f6d87e47cd078c150db2e8771a50e2`
- SDK: `c4dc6d927c65b736d46281d8f3022fcb9960e58fe42b931798703bf4516cb023`
- Default diagnostic ELF: `e29017ad6370a02d90bbdb78e2389375cc6b5fecb319a1fcda747a8e9fa4bc6f`
- Result-error diagnostic ELF: `0f366b95a9ac7cebe577cad3bfbdfb1564c0ca9c7fc626e6b6053f1eb884d0fb`

Actual std observations included File/OpenOptions create/truncate, shared clone
cursor and nonfinal close, independent append, metadata/timestamps, read/write,
seek/set_len, zero-filled growth without cursor movement, actual sync,
BufReader/BufWriter, a buffer boundary splitting a UTF-8 scalar, invalid-UTF-8
refusal with subsequent raw-byte recovery, and create_new/missing/read-only/
invalid-offset refusals followed by reuse. Both clean second boots read saved
std UTF-8 bytes before rewriting, as well as earlier native SYNC bytes.

Ordinary GUI launch used the genuine bound Documents alias; current_dir,
relative paths, parent/root traversal, explicit chdir, failed chdir preserving
the old directory and subsequent reuse were observed. Original native bounded
file/preflight/prepared-open cases repeated. This does not yet qualify the new
policy in initial or replacement Finder consumers.

GUI stdin returned immediate zero-byte EOF without changing its input buffer;
stdout/stderr accepted arbitrary bytes with full successful counts, successful
flush and nonterminal status. The default variant exited normally with code 0;
`main() -> Result::Err` exited normally with code 1 and no panic/report, with
stderr intentionally discarded. All four actual Process/account removals,
Finder refresh/command-close/input and reviewed final screens were observed.
Sources/base images stayed fixed, owned guests exited zero and private generated
media was removed.

Two earlier combined runs remain failures. The second identified a WRITE refused
at initial admission because an existing Finder icon group was live. The new
canonical waiting implementation allows that pre-existing group to finish before
acquiring file mutation exclusion; it does not retry committed effects in std.
The subsequent finite successful runs do not establish adversarial scheduling,
general Save/transfer overlap, pressure or cancellation safety.

Directory iteration, directory creation/removal/rename/copy, permission/time
mutation, locks, symlinks and canonicalize remain explicit Unsupported in this
std backend. Provider-path support, current_exe/environment/child-process work,
late failure, power loss, sustained concurrency, exhaustion, cancellation,
rollback failure, SMP and other hardware remain separate qualification gates.
This is not completion of all std APIs or final application/main-tree integration.
