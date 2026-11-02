# Native application runtime and allocation

The installed no_std RV64 applications use the `mac-toolbox` application feature and one safe
entry declaration. The SDK owns service installation, the Rust global allocator,
panic reporting and allocation-failure reporting. Drivers, extensions and
WDEF/CDEF definitions retain their own runtimes; an application runtime is not a
module allocator or a replacement System allocator.

```toml
[dependencies]
mac-toolbox = { workspace = true, features = ["application"] }
```

```rust
#![no_std]
#![no_main]
extern crate alloc;

use alloc::vec::Vec;
use mac_toolbox::{System, ToolboxResult};

fn run(system: &System<'_>) -> ToolboxResult<()> {
    let mut values = Vec::new();
    values.push(42u64);
    system.log("Application entered with its native runtime.\n")?;
    Ok(())
}

mac_toolbox::application!(run);
```

This provides `alloc`, not a Rust `std` platform implementation. Filesystem,
threading, TLS, synchronization, I/O and the actual target/sysroot integration
still require their own contracts and native qualification. The runtime
currently targets RV64; unsupported application targets fail compilation rather
than acquiring a host allocator or replacement service gate. Application source
needs no raw entry pointer, unsafe allocator implementation or panic handler.

## Required provider services

Native application build manifests (including Finder) can declare exact provider requirements separately
from NABI's executable/ABI description:

```json
"required_services": [{ "class": "ENTR", "version": 1 }]
```

The compiler emits one `NREQ` resource with ID 0 and zero attributes. An absent
or empty manifest list emits no resource. `mac-formats::requirements` owns the
allocation-free decoder and encoder used by the packager and inspector. A class
is the existing four-byte `mac_module::ClassId`; a version is an exact nonzero
`u32`, not a range. Unknown nonzero classes remain valid metadata for external
providers. Duplicate classes, including conflicting versions, are refused.

Schema 1 uses a 16-byte header: `NREQ`, big-endian `u16` schema, big-endian `u16`
record count, and eight zero reserved bytes. Each eight-byte record contains the
four class bytes followed by a big-endian `u32` version. There are at most 32
records (272 bytes total); length must match the count exactly. Unsupported
schemas, zero class/version, nonzero reserved fields, duplicate resources or an
NREQ under another resource ID are errors. Build manifests must use the declared
list rather than supplying raw NREQ resources. Lossless resource archives still
preserve their original bytes, including malformed metadata.

## Process-scoped provider admission

Normal native launch, initial Finder and replacement Finder decode the same NREQ declarations before author entry. Selection and dependency publication happen in one provider-registry turn, binding an exact interface incarnation to the genuine Process TLRT scope. Repeated admission reuses that exact binding or refuses it; it never selects another provider after withdrawal. Dependencies are charged to the original Process Kernel account.

The TLRT scope belongs to the actual runtime Root, not its long-lived directory selector. Real runtime use keeps it open; closing author admission alone does not acknowledge it. DDEP retains a scope lease without retaining Root, avoiding an account-close cycle. Cleanup keeps the unique dependency ticket while the scope remains open, and actual closure/removal wakes both Process retirement and dependent module recovery. Prepared-root publication binds its reservation in the canonical owner-ledger turn and returns redundant pins for disposal after manager guards.

Provider selection establishes registration availability and exact-version agreement.
For ENTR1, launch also submits the real provider READY operation before author
publication. The ordinary-launch continuation retains the original prospective
Process, loaded package, heap/TLS roots and exact provider job across the actual
launcher's park/replay. Document commits retain their batch identity and propagate
the same typed refusal on replay. Caller stop before publication acceptance
abandons the launch; acceptance and cancellation share one bounded slot turn.
A later publication failure still retires the prospective owner.

Initial Finder retains those original resources in an owned startup continuation.
A sealed pending token permits the genuine System idle/scheduler path while the
provider is pending; canonical driver-job wakes resume startup maintenance.
Only READY completion publishes Finder's thread and desktop. An initial refusal
enters the minimal startup-failure UI, retaining original boot ownership until
machine reset. Replacement Finder uses its existing recovery coordinator and
retains its admission and exact job through READY or cancellation.

No manager, image or Desktop guard crosses a provider callback or wait. Queued
requests carry genuine runtime activity through completion/cancellation ACK;
dropping a launch continuation does not release that activity early. Public
entropy requests reuse the runtime's exact scoped dependency, admitting it lazily
for a caller without NREQ; they cannot reselect another provider after withdrawal.
Readiness is an admission observation, not a promise of future device health.
Installed applications still use no_std. The std SDK application packager adds
ENTR1 to the same NREQ manifest compilation automatically, reusing an identical
declaration and refusing an incompatible version. This does not add a requirement
to no_std applications or modules. Generic low-level native packaging does not
infer a runtime from arbitrary ELF input. Private packages below exercise the
launch paths before installed-app migration.
Direct Process launch and document-commit refusals carry the required class/version through their respective call records and the public typed ServiceRequirement error. Finder obtains its explanatory template from resources. Existing numeric status values and development ABI version1 remain; the changed record fingerprint requires rebuilding every client. The DocumentCall adapter clears failure-only outputs before entry and accepts them only on a failed DOCUMENT_COMMIT. Both transports share the same checked payload decoder. Finder document associations and application drops use DocumentOpen::commit and retain the typed error through the existing resource-based alert. Other Open wrappers do not automatically gain this payload.

### Bounded native observations — 2026-11-01

The optimized release/LTO RV64 System resource
`4fdee4dda1cb59b0f4c59a7f7e9c25e121e773cb7b2c053862f3aceeeb1cd5ff`
with ROM `a85d5a5ce9b2de9c267493d87836aac580c8283de075ce6da90c7bb1ec324b60`
ran in private 8 GiB, four-CPU QEMU guests with event/TextEdit tracing disabled.
Real packaged no_std Thread Probe declared POWR1 and observed one TLRT root and
one scoped DDEP through the public ledger surface, then exercised its existing
child/TLS/heap/wait/join workload and actual THRD/Process/account closure.
Separate partial POWR1 then MISS1 and wrong-version POWR2 packages each refused
twice before author entry, reclaimed the unstarted Process, displayed the required
class/version in the real Finder alert, and allowed dismissal/retry and input.

A real Finder-role fixture declaring POWR1 completed initial entry and two SDK
Abort/replacement cycles. Old scopes retained their DDEP cleanup tickets while
threads were still alive, then actual THRD and Process/account closure completed.
The replacement and independent survivor accepted input. This exercises the
replacement coordinator, not production Finder OOM restart or complete report
preservation under exhaustion. Source and base media stayed fixed during each
capture; owned guests exited/reaped and cloned media were removed. An initial
host collector failed on interleaved serial text despite the native refusal and
reclamation; the failure is retained separately from the corrected capture.

These are finite production-path scenarios. They do not qualify provider
withdrawal races, asynchronous READY completion, resource exhaustion, all Open
transports, race absence, SMP safety or latency. Guest assertions and logging
perturb execution; no timing claim follows from these observations.

## Standard-library startup

The staged std application target uses a platform-owned startup instead of the
application feature. Authors provide ordinary `fn main()` and acquire the same
services with `mac_toolbox::System::new()`. Do not combine that target with the
old application entry/allocator/panic macros. Build it with the pinned private
[std SDK](../platforms/rust/README.md); this is not yet the installed-app default.

Std startup publishes its validated service table before Rust initialization,
checks the actual canonical current thread, and enters the compiler's main
wrapper. Main return runs shared native TLS teardown then signed Normal exit.
Explicit process exit skips user TLS destructors. The default panic and allocation
error hooks report through System independently of stdio, then request Abort
immediately. System owns dismissal and actual quiescence/reclamation; failed
author code does not wait on the viewer. Early/recursive failures use the shared
core-only emergency Abort transport. See [current std evidence and remaining
gates](RUST-STD.md#current-evidence-and-gaps) for the bounded native observations.

## Heap admission

The loader prepares an empty canonical application heap root before entry.
This admits its real owner/lifetime metadata, not an initial payload extent.
The SDK validates and installs the permanent service table before calling author
code. One publication supplies both allocator and failure routing; there is no
heap-initialization RPC, second current-app selector or shared last-heap-error
variable. Invalid entry tables return a typed status to the native entry caller.
Repeated installation does not silently replace an existing table.

A Rust allocation uses the actual resident page-backed Application heap. Its
minimum growth starts at 64 KiB, configurable within 16 KiB–2 MiB by platform
policy. The SDK forwards exact requested layouts without independently rounding
or substituting storage. Allocation-free applications acquire no buddy extent
merely by linking the runtime. Resident execution supplies application/account
authority; requests contain no application ID, account or heap selector.

Current application addresses remain in trusted shared supervisor mappings.
An application-owned heap and checked service ownership do not establish U-mode
process isolation or a private address space. Legacy Memory Manager handles keep
their separate interface and wire format; they are not the Rust allocator.

The ABI uses `APPLICATION_HEAP` (`0x10028`) with one exact native Call record:
allocate, allocate-zeroed, deallocate and reallocate. On RV64 the record is 248
bytes, with 192 bytes of bounded UTF-8 error detail. Result/detail fields are
outputs; their input contents grant no authority. Only successful committed
allocation/reallocation publishes a nonnull aligned result. Disposal and failure
return no allocation. Zeroed allocation clears the requested bytes in the real
resident service. Raw-pointer methods in `mac-traps` have explicit unsafe
ownership/layout obligations; ordinary applications use Rust collections.

Only a well-formed resident NO_MEMORY result becomes a null GlobalAlloc return.
Failed reallocation preserves the old allocation, layout and data. The SDK never
frees it first, retries through System storage or guesses another allocator.
Foreign pointers, invalid layouts, Busy, authentication errors and malformed
replies retain their actual per-call diagnostic and enter the application failure
path. A dropping caller does not acknowledge resident cleanup. Safe Rust owners
prevent ordinary double disposal; stale raw-pointer use remains an unsafe caller
violation, not a generation-tagged handle guarantee.

## Failure reporting

The default panic and infallible-allocation failure paths stream bounded stack
buffers to the System-owned crash service. OOM includes the exact requested
layout and NO_MEMORY identity. A non-OOM allocator failure includes its actual
status/detail. Reporting needs no application String or Vec, does not redraw
through author code and does not allocate another block to explain exhaustion.
System retains the dialog, event handling and report; complete reporting under
all pressure conditions still needs its own native evidence.

A returned ordinary Toolbox error is logged with its typed cause/status and
returned to the native application caller before process exit. Fallible Rust
collection operations may return their ordinary allocation errors to application
code. They do not imply that every application operation recovers its UI state.

Before validated services exist, one audited native boundary uses existing
LOG and PROCESS EXIT ECALLs with live fixed buffers and the actual executor.
If the genuine nonreturning EXIT returns, the runtime logs that violation and
raises a deliberate terminal architecture fault. It does not silently spin or
claim contained recovery. Recursive/concurrent failure during reporting follows
the same explicit termination discipline; a future multi-thread application
failure coordinator is not established here. **M-D04:** arbitrary privileged-
fault containment is **unproven** and is a **required future gate**.

## Reactor Room's first allocation consumer

Reactor retains visited pool/provider and completion-fault rows in sparse sorted
Vec caches. Keys include actual membership epochs, owner kind/incarnation and
ledger revision. A hit avoids repeated metadata traps; a miss reserves capacity
before its real metadata read. Allocation may change Reactor's own ledger, so a
stale read retries honestly while retaining useful capacity. Refresh clears
invalid rows without freeing and reallocating the same backing. UI caches are
thread-local application data; no cache borrow spans inspection/resource calls.

The caches grow fallibly under the actual application budget, without an
arbitrary row maximum or eager catalogue prefetch. Managed scroll boxes retain
only visible text rows and repaint their changed bounds. A cache-growth failure
returns NoMemory through the application's existing typed error path; complete
view recovery under pressure is not claimed. Quit/view disposal uses ordinary
Vec ownership and the same resident deallocation path.

## Bounded native observations — 2026-10-29

The optimized QemuRISC-V/RVA23 build ran in private QEMU virt guests with 8 GiB
RAM, 800×600 display, event/input tracing disabled and one or four physical CPUs.
The shared System resource SHA-256 was
`56553d920e9f0791191400c7d6b3c6e5df023966cc3d5f811dbfd404d8e5e952`;
System ELF was
`56a9fa7d0f9659dc8d3dcd4d744ce67619049925f86229ac73da54de3aa76c8f`;
boot ROM was
`26ea40c308f8ba73bd09b7ff64954173e3e563dcc4f00d7f332e45ec75bb033a`.
The ordinary packaged Heap Probe variant of the existing native Process Spinner
fixture used this runtime, public SDK and actual resident allocation traps.
No host-linked service or replacement allocator supplied these results.

One application on one CPU and two distinct application files on four CPUs
each exercised Vec growth from 98,321 to 655,391 bytes, shrink to 32,775 bytes,
full retained-data comparisons, a 65,560-byte zeroed u64 allocation and 1,024
varied-size replacements with up to eight live vectors. A valid request larger
than the actual application budget returned typed NoMemory with resident UTF-8
detail. The failed growth preserved the previous pointer, capacity, length and
contents; account commitments/reservations did not change. A following valid
allocation succeeded. The two four-CPU application lifetimes overlapped; that
alone does not establish simultaneous allocation instructions on different CPUs.

After ordinary drops, each application's committed Application bytes returned
to its actual 4 MiB stack baseline, reservations were zero, and the allocation
pool had zero live/pending/quarantined/faulted entries with 1,027 births and
deaths. Each application then deliberately retained one 160 KiB vector across
normal return, suppressing that vector's Rust destructor. Its live allocation
and additional charge were observed before exit; actual thread reclamation and
PROCESS_EXIT followed. That production exit marker occurs only after thread
quiescence, settled cleanup, empty stopped ledger and successful account closure.
This exercises the System walk of remaining heap ownership in those scenarios.

The same System/ROM also ran all seven bundled application entry paths, normal
own-quit paths for the six non-Finder applications, and a 27,246-byte/331-line
SDK panic report with System-owned scrolling, dismissal and process retirement.
Reactor's actual cached-row views on one and four CPUs showed its live AALC and
AAST entries, then quit and accepted following input. Native app/worker migration,
register/service checks, normal and failed-initialization worker reclamation,
direct-worker Toolbox refusals including APPLICATION_HEAP, and policy
demotion/retirement were exercised again on this same artifact.

These are finite native scenarios. Pattern comparisons, phase summaries,
physical PC/SP observations and injected input affect execution; they establish
no latency bound or race absence. Broad alignment families, hostile raw-pointer
requests, allocator contention, all stop/pin interleavings, complete OOM UI
recovery, private process address spaces and application std remain unqualified.
In particular, allocation copying/zeroing is outside heap metadata locks but
still inside the synchronous trap's interrupt discipline; large-operation input
latency has not been measured. All owned guests were reaped and private cloned
media removed; the user's installation was not replaced.

### Document-commit requirement transport

DocumentCall includes failure-only requirement_class (four bytes) and
requirement_version (u32), both zero outside a declared-service launch refusal.
The resident clears them before validation and copies actual Process launch
failure identity into the commit reply. Native callers validate operation, failed
status, nonzero class/version and status width before forming ServiceRequirement.
Partial identities or success replies carrying error identity are BadAbi.

The whole-world layout fingerprint includes DocumentCall size and both field
offsets. Development ABI remains1; System, apps, modules, definitions and SDK
clients must rebuild together. This source transport is not proof of asynchronous
provider readiness or correct pending-document replay. Its updated native wire
was exercised separately in the bounded observations below; previous artifact
observations above remain scoped to their recorded earlier layouts.

An optimized QemuRISC-V whole-world build ran in private QEMU
virt guests with four CPUs, 8 GiB RAM and event/TextEdit input tracing disabled.
System resource SHA-256 was
`2f23cf112f75dda00feef4b4af3903581cd2f7eb2a742d8083a2b3b5b73cc629`;
System ELF was
`9c3629856fb18ed77478a023e783c4a94eec52750a64415459212087287774c8`;
boot ROM was
`831c68a54582b71736b0b5f45853fc9c7311a1b958610614c5115088380e4708`.
The actual packaged Finder opened a TEXT/TEDT document through its association
and public DocumentOpen::commit. The actual rebuilt Text Edit package declared
either absent MISS1 or incompatible POWR2. Each variant refused twice before
author entry, retained class/version and cause in the System-owned Finder alert,
closed the original unstarted Process/account, accepted dismissal and retried.
A direct application-open variant also retained MISS1 through the common decoder.
Following mouse input was observed after the refusals. Source/base artifacts
were unchanged; owned guests were reaped and private media removed.

These finite scenarios cover the synchronous declared-provider refusal paths
and updated native transport. They do not establish asynchronous READY ordering,
pending-document replay/cancellation, exhaustive malformed-wire rejection,
latency or race absence. Existing diagnostic output, screenshots and injected
input affect execution. No host-linked service supplied these observations.


### Build-freshness claim correction — 2026-11-01

The earlier “checksum-fresh” label for the recorded builds was unsupported.
Their recorded feature flag alone did not establish content-based freshness.
The exact artifact hashes, native observations and unchanged-source receipts
remain their evidence; they do not prove that every changed source was rebuilt.

The build now requires both `CARGO_UNSTABLE_CHECKSUM_FRESHNESS=true` and
`CARGO_BUILD_FINGERPRINT=content`, followed by checking the actual Cargo
fingerprints and artifact hashes. Build-script inputs still need explicit
rebuilding when their mtime tracking misses changes; content mode does not
remove that separate limitation. See the [Cargo freshness contract](https://doc.rust-lang.org/nightly/cargo/reference/unstable.html#checksum-freshness).


### Bounded READY observations — 2026-11-01

Optimized release/LTO RV64 System resource
`faccb1df3c35b79cb1454f84b759bc7e1e2dbc7dbefe214e114ed4c977639006`,
ROM `73954623bc3ed093a15ddce337473768263e14dfc748cf5cfda39be07fbf1a09`
and matching std SDK `056bc07dc1d2272fed1caf2c3a318a0b87ff16eee8a6ed434f6c39bdb04a58a3`
ran in private 8 GiB, four-CPU QEMU guests. Holding the actual virtual RNG source
left a real descriptor outstanding during initial production Finder READY; all
four paused CPU samples were in genuine System idle, input responded, and no
Finder thread had been published. Source release led to Finder entry/desktop.
Ordinary launch exercised pending identity, caller termination, original unstarted
Process/account closure, actual device reset and a fresh successful retry.

A packaged production Text Edit declaring ENTR1, opened through the real Finder
association, remained unstarted while READY was pending; source release opened
the actual document. A private debugger-injected RNG DeviceStatus FAILED bit
then exercised actual driver refusal/reset, typed document replay error, unstarted
account closure while source was still withheld, alert dismissal and successful
source-backed retry/document/quit. The final Finder resource
`d1b2a1237d0f56b2067ae862be729ebf5a727f3b2df6392ce6169844a193eea2`
displayed the required ENTR/version and the corrected general I/O cause. The
original capture's storage-specific wording failed informative-cause review;
a collector prematurely started during rebuilding also failed and is not counted.

The same bounded device-status injection during initial Finder READY reached the
startup-failure screen, retained unique original boot resources until reset, and
its real Restart action reached a fresh Finder desktop. Two ENTR1 Finder-role
fixture replacements exercised the recovery coordinator with an already-ready
provider; they were not production Finder OOM or pending replacement cases.
Actual THRD and Process/account closure and subsequent input were observed where
applicable. Source and base media were fixed per accepted capture; owned guests
were reaped and cloned media removed.

Cargo's actual guest records selected content fingerprints with both the feature
and content-mode settings; generated/resource inputs received explicit rebuilds.
This corrects earlier feature-only freshness guidance: that flag alone left this
Cargo's default mtime mode active. Earlier artifact observations still describe
what ran, but do not establish freshness of a later source revision.

Logs, paused CPU/device-metadata observations and the private debugger channel
perturb execution. Device-status injection does not qualify spontaneous failure,
malformed DMA, source security, timing, race absence, arbitrary fault recovery,
provider-withdrawal races, pending replacement cancellation, stalled-source
deadline policy, exhaustion, Windows
or other hardware. These finite cases do not establish full std adoption.


### READY with owned Process page roots — 2026-11-01

The launch continuation admits the genuine Process page root after READY and
before author-thread publication, outside Desktop. Immediate and delayed initial
Finder use that same admission boundary. The sealed pending-startup route selects
and activates the System root before entering the actual idle frame; it retains
no application-root pin on the abandoned bootstrap stack. Replacement Finder
continues through its existing owned recovery preparation.

An optimized release/LTO, four-CPU, 8 GiB QEMU image combined this continuation
with the independent supervisor Process roots. System resource SHA-256 was
`f785ef9ac17812dd24ba66e0cc979d42e24b70462f5dde8bc49c67151f2f5878`,
System ELF was
`7ffdff68724a279e98838077d82bc815c064c6eed032b72db872c948d8b55047`,
and ROM was
`ddf4e7d953b20de364ee9abe4acd08882a823eb43dc35a420d69370e0b3bdc05`.
The build used both Cargo freshness settings, actual content fingerprint records
and an explicit System-package rebuild for generated inputs.

The actual Finder ELF was packaged with ENTR1. With the real virtual entropy
response withheld and an actual descriptor outstanding, all four paused CPU
PC/SP/SATP observations were in the genuine System idle body on System root
`0x8c802000`; no Finder thread or author entry had been published. Input responded.
Releasing the source led to actual Finder entry and desktop. A production Finder
document association likewise held the actual Text Edit package before READY,
then opened its document and reclaimed its thread and original account on quit.

A separately labelled private virtual-device fault set only the verified RNG
DeviceStatus FAILED bit through QEMU's physical debugger channel. The real
driver reset and acknowledged the failed operation while the source remained
withheld. The original unstarted Process/account closed; Finder displayed the
typed ENTR1/general I/O cause. Dismissal and a fresh source-backed retry opened
the document, followed by quit, actual reclamation and input. No guest RAM,
PTE, CPU register, entropy payload or seed was written by the observer.

Sources and base artifacts stayed fixed during each observation; owned guests
and helper processes were reaped and cloned media removed. Paused metadata/CPU
observations, injected input, logs and the fault channel perturb execution.
These finite cases do not qualify arbitrary faults, races, timing, full std
children on this combined image, pending replacement cancellation, exhaustion
or production Finder OOM restart. Applications remain trusted supervisor code;
these roots do not provide user-mode isolation.
