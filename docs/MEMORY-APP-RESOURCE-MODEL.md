# Memory, application and resource model: first-cut design

Status: §13.1 accepted by Chloe, 2026-10-23; proceeding with §13.2. The complete
RVA23 floor is live. Process identity/directory migration has begun; the live
allocator, paging, complete owner-walk cleanup and scheduler-capacity migration
remain pending. This document describes the implemented foundations,
architectural decisions and remaining migration gates.

## What landed

The `no_std`, `forbid(unsafe_code)` **mac-kernel-model** crate is internal
kernel infrastructure, not an application SDK. Keeping pure structures in a
host-buildable crate permits checking arithmetic, ownership and allocation
without emulating a CPU or adding test-only System traps. System now consumes
its execution-use gate; other families remain staged until their resident
consumers land. Linking the crate does not activate its page or allocator model.

| §13.1 family | Code | Current scope |
|---|---|---|
| Buddy pool | `crates/mac-kernel-model/src/buddy.rs` | Caller-supplied metadata and order heads, split, O(1) unlink, coalescing, unique RAII block leases; never touches free-page bytes |
| Page layer | `…/pages.rs` | Platform geometry, typed addresses, permissions/ownership, borrowed boot reservations, largest-leaf mapping plan, reserved `PageBox`/address-space root |
| Domains/accounts | `…/policy.rs`, `mac-object-table/src/domain.rs` | Boot-monotonic incarnation, account/reserve records, checked registry construction; no live allocator charges yet |
| Facility v2 | `mac-object-table/src/domain.rs` | Owner-index-authorized per-kind bitmap pools, generations, rotation, retirement, capacity policy, release tickets and registration shapes; managed controls use owner-backed value storage |
| Shared aliases | `mac-object-table/src/aliases.rs` | System-owned targets and per-process registered aliases, validated sharing, last-alias release and retained target cleanup; executable pins are the first resident consumer |
| Process/thread | `…/process.rs` | Address-space, image/memory/legacy capabilities, dynamic threads, saved manager slots, backend frame, vector marker and discovered save-area layout |
| Execution use | `…/image.rs` | Nonblocking load exclusion, fallible gate allocation, atomic load-to-pin handoff; real scheduler records retain pins until stack reclamation |
| Control zone | `…/zone.rs` | Same-owner instance/zone binding, configured zone header, host-forwarding allocator contract; no allocator installed |
| Uniform layout | `…/policy.rs` | One checked range record emits named linker symbols; no unapproved addresses or duplicate linker constants |
| TE2 rope | `…/text.rs` | Immutable balanced chunk tree, byte/hard-break augmentation, shared snapshot roots, logarithmic chunk lookup; no edit/rebalance/control/file-provider migration yet |
| Emergency kit | `…/emergency.rs` | Borrowed, checked requirement manifest and aligned size calculation; no failure path claims until backing kit is installed |

The shared facility remains the sole active ID issuer. Process identity now
uses its complete u64 domain incarnation and a growable owner directory, retaining
the sole owner index beside the process payload. Directory lookup is binary
search, and clients cannot mutate/swap a ledger through a payload reference.
Retirement refuses a nonempty ledger; invalidation yields cleanup tickets for
dispatch outside the lock. The old Processes namespace, capacity and shared
generation array are removed. Managed controls now use opaque native u64 tokens resolving to these owner keys;
remaining active kinds still use their existing
u16-generation/one-based-slot wire layout. The new internal key includes the
domain incarnation; native/legacy object projections are a later explicit
migration. Legacy handles/refnums project through this same facility; never
truncate a ResourceKey generation/incarnation into a smaller wire field. A
projection must preserve its own generation history/retirement and resolve only
to a currently valid key, while original 68k appearance stays intact. No second
manager-local ID issuer is authorized. An index cannot recreate a kind pool or be recreated with a retired
incarnation. No public mutable pool reference permits swapping another owner's
pool into the index. Exhausted generations retire; metadata needing growth is
reported separately from quota exhaustion. Allocation growth has no implicit
initial size or hidden growth quantum.

The directory now retains a fallibly allocated, shared `OwnerRef` for each
ledger. Managers clone that authority instead of creating another index. Its
lock is private; every access runs inside the explicitly selected architecture
`InvocationGate`. The native adapter masks/restores local MIE with compiler
memory barriers, also replacing the old I/O-local guard. No callback runs under
the metadata lock. Empty-check plus admission closure is atomic at retirement;
directory Drop also stops new issuance through orphan references, while leaving
existing keys for RAII cleanup and the explicit dispatch walk. A sealed ledger
view lets binding validation consume owned or shared authority, and refuses
stopping domains with an error naming the domain. Retirement discards the empty
domain's bitmap/generation arrays immediately; surviving references retain only
a closed identity stub, not the dead pool's storage.

The existing resident heap now has a private `ResidentHeap` wrapper using the
same architecture gate for allocation, deallocation, initialization and free-
space reads. This closes the preempted-heap-holder path without changing the
current 8 MiB backend or claiming the later page-backed/accounted heap stage.
Trusted event routing also requeues directly into the fixed queue, avoiding a
temporary heap allocation on every physical-input batch.

The first Reactor Room slice now consumes the public inspection service. Directory
membership and each atomic owner-ledger row have separate nonwrapping revisions;
successful handle publications/invalidations update cumulative counters at their
existing mutation points. Reads allocate nothing and never scan object values.
Cross-domain detail is authorized by a generated System ACAP policy bound to
canonical application resource digests; ordinary callers retain own-domain reads.
The policy is not a gate on public chain/claim/mapping registry existence.
Reactor Room explicitly reports process/module domains with partial-ledger coverage. See
[Reactor Room staging](REACTOR.md) and [the policy wire format](../FORMAT.md#application-authority-policy-acap-0-development-abi-1).

This is the start of §13.2/2, not completion: other managers do not yet insert
their objects into process ledgers, and the EXIT/rollback cleanup lists and
fixed-eight scheduler/heap/image storage still exist. The real process fixture
checks native and Pascal stale IDs across relaunch, fresh incarnations, shared
app-menu actions and full-width window resource ownership. The directory's
greater-than-eight test is host metadata coverage, not a claim of greater-than-
eight live applications or MMU isolation.

`ReleasedResource` records invalidation-before-dispatch. Registered controls now
use the live dispatcher described below; this is not a proof of quiescence for
the remaining managers. Alias pinning and account charging remain integration
work. A drained observation alone cannot authorize page reuse.

## Architecture and platform boundaries

Common code owns domain identities, accounts, pool metadata, object lifetimes,
text augmentation and layout policy. It has no CPU assembly, register numbers,
PTE masks, `satp`, `sfence`, hart lookup, MMIO or global page-size constant.
Physical/virtual addresses are distinct; conversion to a usable mapped pointer
belongs to a narrow audited adapter. They are u64 address values, not host
pointers or a promise that all target CPUs can map the full u64 range.

The compile-time `Architecture` contract supplies base-page geometry, supported
leaf sizes, canonical-range validation and concrete root/frame types. The
future `system/src/machine` architecture adapter supplies PTE encodings including
ownership software bits, ASID allocation/TLB shootdown, CPU-local current-context
access, System-stack entry, context/vector save and activation. Machine/board
adapters supply RAM/reservations, devices, timers and interrupts. The backend
must reject an unrepresentable layout, never silently truncate it. ASIDs are
hardware tags backed by central resource lifetimes, not a second OS object-ID
scheme. The `AsidTag` here is only a descriptor; allocation/reuse/TLB fencing are
not implemented.

No Rust trait-object layout crosses independently linked modules. Architecture
dispatch is static, not a dyn call per page. Extension traits use checked host
adapters at request granularity; see [extension design](EXTENSION-TRAITS-DESIGN.md).

The `PageBox` currently owns **uninitialized physical storage only**, has no
`Deref`, and cannot activate a page table. Initialization/mapping proofs must
precede live root activation. A virtual-pointer wrapper will be introduced with
its page-layer consumer, rather than retaining an unused placeholder. There is
no machine-wide physical/virtual interpretation flag. Xtra's separation
of common MMU ownership from architecture encodings is the reference. Its bump
heap, intrusive free list, per-base-page full-RAM mapper, device model and
scheduler are deliberately not imported. Its low-address-only validation must
not become our kernel high-half validation.

Buddy-pool locking is safe/thread-capable metadata synchronization, not yet an
interrupt/preemption safety guarantee. The resident integration must establish
its IRQ/preemption lock discipline in the architecture invocation wrapper before
using it from traps/interrupts; no callback or heap operation runs under the pool
lock. Host-thread stress is not evidence of interrupt-safe guest allocation.

Large-map planning chooses the largest supported aligned leaf, falling back to
base leaves only at boundaries. Tests describe 8 GiB using eight 1 GiB leaves,
and exercise a different backend with 64 KiB base pages and 512 MiB leaves.
These test geometries are not platform policy defaults.

## Heap proposal for review

Start with a coalescing free-list heap **per page-backed extent**, using the
existing audited allocator's strategy rather than a new TLSF or custom slab
framework. The current resident `linked_list_allocator` already reuses and
coalesces; its *static backing and uncharged ownership* are the problem. Xtra's
bump heap is a separate implementation and is unsuitable.

The fixed System heap reserves its entire physical budget during bring-up.
System-owned allocations use the protected account; process kernel allocations
come from the same backing but require a process charge and sufficient space
above the protected recovery floor. Admission reserves charge before publishing
storage, rollback returns it, and the lease's `Drop` refunds once. This makes a
process limit independent from physical exhaustion and protects Finder restart.
Do not infer the allocation account from whichever module happens to execute.

Application heaps acquire contiguous buddy extents on demand. Proposed growth:
round the request plus allocator/alignment metadata to a buddy order, at least
the configured growth minimum; do not require powers of two for user requests.
Coalesce inside each extent, and return an entirely empty extent immediately.
Never move live Rust allocations. Any later warm-extent cache needs measurements
and an explicit account/limit; it is not a hidden v1 retention rule. Fragmented
free space is reported distinctly from an unsupported request or bad alignment.
System heap cannot grow past its boot-reserved fixed budget.

Box/Vec/String work through the resident global allocator, **but GlobalAlloc's
infallible clients cannot report Result**. Before the heap switch, audit every
app-paced resident growth and independently linked module allocation. Such
operations must reserve fallibly and preserve typed causes. Merely changing
`allocation_failed` cannot make an infallible Rust allocation recoverable, and
abandoning a stack with live locks/RAII values is not recovery. A platform
panic/unwind/recovery contract must be proven before claiming module panic
containment. Contained failures are typed errors, contract-level refusals, and
faults covered by the proven panic/unwind contract. For contained failures, a
mid-call failure fails the call typed, quarantines the registration, and degrades
the service to `NotAvailable` according to its contract.

**M-D04:** The walk provides resource containment. Arbitrary privileged-fault
containment is **unproven** and is a **required future gate**. Until a recovery
contract exists and passes, the System must not claim it can safely recover from
every mid-call module fault.

The unsafe backing-pointer bridge belongs to one audited allocator
adapter; accounting/free-list policy stays safe.

## Configurable starting defaults and remaining profile proposals

The initial configurable defaults approved on 2026-10-24 are **8 GiB native
release VM RAM, 512 MiB fixed System heap, 32 MiB protected recovery reserve,
16 MiB per-process kernel budget and 64 MiB app committed-heap budget**. These
are selections for the later page/heap stage, not claims that those budgets are
active on the current 8 MiB allocator. They remain tunable and must pass the
cross-profile validator and live accounting/recovery gates before activation.

Other values, including the proposed minimum/maximum ranges below, remain
**review proposals**. Evaluate them with real image/control/account usage,
including Res Edit and a 200 MB Text Edit document, before enabling. Approval of
the five starting defaults does not select virtual addresses, stack/control-space
sizes, Finder's recovery share, or emergency-kit concurrency.

| Configured policy | Minimum / initial default / maximum proposal | Reason |
|---|---|---|
| Physical RAM for native release VM | 512 MiB / 8 GiB / platform-discovered | Target class is ~8 GiB; small guest remains a deliberate test profile |
| Fixed System heap | 128 MiB / 512 MiB / 1 GiB | Must hold resident state plus charged process kernel allocations, including high-water reserve |
| Protected recovery floor within System heap | 16 MiB / 32 MiB / 128 MiB | Measured kit + Finder restart, unavailable to ordinary process charges |
| Finder restart portion of that floor | 8 MiB / 16 MiB / 64 MiB | Subset of floor, never added twice; final value measured from restart peak |
| Process kernel account | 4 MiB / 16 MiB / 128 MiB | Controls, tables, zones, TCBs and frames; admission also checks global remainder |
| Process app committed heap budget | 4 MiB / 64 MiB / 2 GiB | Grow on demand; a 200 MB file uses file-backed views, not a forced whole-file copy |
| Application heap growth minimum | 16 KiB / 64 KiB / 2 MiB | Rounded up to target geometry; bounded by selected process budget |
| Per-thread fixed stack | 64 KiB / 256 KiB / 8 MiB | Static per thread, charged app account, freed after switch-away |
| Designated per-control space | 1 MiB / 16 MiB / 128 MiB | A sub-budget of process kernel account, never independently guaranteed physical memory |
| TE2 owned threshold | Tunable 0–100%, default 40%, bounded by a tunable **~3.2 MiB default ceiling** | Ratified 2026-10-23 calibration: owns strictly below both limits; at either limit uses view. The ceiling is 3,355,443 bytes (3.2 MiB rounded down), even if control space grows |
| Process/thread/handle counts | Grow until account or memory refusal | No fixed-eight or guessed high replacement constant |

Selected values must satisfy cross-profile constraints: reserve <= fixed heap,
Finder portion <= reserve, control space <= kernel budget, growth <= app budget,
stack budget available before spawn. The current constructors do not select a
profile, and no complete live profile validator is claimed yet.

Uniform layout proposal: one app interval/preferred image VA for all processes;
private supervisor process-kernel interval; shared supervisor module interval;
shared supervisor kernel interval containing the physmap. Store the chosen
ranges once, validate through the architecture, generate both Rust input and
linker symbols in xtask, then rebuild all producers. **Concrete VA values and
healthy range selections remain [DECIDE]**; don't fossilize today's physical
0x81… application addresses as future virtual addresses. The emitter exists;
production linker scripts have not switched to it.

Image resource residency proposal: **resident-with-image** for v1.
Last image release drops code/resource backing
immediately; no unaccounted warm cache. Shared immutable image backing plus
per-process writable sections must replace today's never-unloaded `Once` slots.
Demand paging is a separate [DECIDE], not an implicit change here.

## Emergency services: draft enumeration for ratification

Heap initialization must be immediately followed by kit reservation and pinning,
before fonts, modules, Finder or ordinary service allocation. Failure to reserve
the kit is a boot admission failure with a minimal ROM/framebuffer report.

| Service | Preallocated contents / zero-allocation obligation |
|---|---|
| Fault / System panic | Trusted bounded diagnostic record, formatting scratch, bomb image/font, minimal display/input state, Restart action; truncate visibly rather than allocate |
| OOM report | Account/domain/request details, event slot, core alert resources and scratch; never enumerate all objects just to display refusal |
| Ledger teardown | One-at-a-time invalidation tickets, bounded failure-report slots, drain/cancel bookkeeping; no allocation proportional to object count |
| Bootstrap / watchdog | Runnable recovery state, per-CPU emergency stack/context, scheduler fallback storage; independently usable from failed policy |
| Save changes | Already pinned DLOG/controls/strings/fonts, modal state and bounded filename display; cannot depend on the dying app's heap |
| Save / Save As | Dialog/control/scratch records and a bounded viewport/page of directory results, provider request/cancellation records, filename entry buffer |
| Core alerts | Resource-backed error controls/icon/font and preallocated input/tracking state |
| Finder restart | Separately protected admission/account allowance plus minimum launch/stack/image references; stop failed generation before relaunch |

The manifest records per-service bytes/alignment and rejects duplicate entries
and overflow. It does not assert that these paths are allocation-free today.
The heap stage must enumerate each transitive allocation and prove worst-case
scratch/event/provider demand with allocators refusing all new allocation.
A Save dialog can still encounter a provider requiring memory: the reserved UI
must remain usable, preserve the unsaved document, and report that refusal.
Zero-allocation reporting does not promise every storage operation can succeed
under global exhaustion. Healthy kit sizes and concurrent-use policy need
ratification after measurement; no magic emergency buffer sizes are chosen now.

## Divergences and required resolutions

| ID | Evidence / conflict | Proposed handling; no silent resolution |
|---|---|---|
| M-D01 | `bootrom/src/main.rs` reports fixed 128 MiB; BootInfo has no reservation list; §3.1 assumes both discovery and reservations exist | Add discovered RAM plus borrowed/shared reservation transport before enabling pool; metadata storage itself must be reserved |
| M-D02 | Module mapping wording was ambiguous | **Resolved, Chloe 2026-10-23.** Each process owns its address space/page tables, including its own module/kernel mapping entries. Only immutable code frames are shared; writable state belongs to module-domain pages. No app-owned state in the module region. All module mappings are supervisor-only, entered by System under module context |
| M-D03 | §3.7 places dynamic processes on page layer; §13.2 enables dynamic capacity earlier | Migrate dynamic metadata on current backing first if useful, but page-backed domain enforcement waits for page activation; do not claim isolation or memory-scale acceptance early |
| M-D04 | Resource containment vs privileged-fault containment | **Accepted, Chloe 2026-10-23.** The walk provides resource containment. Arbitrary privileged-fault containment is **unproven** and is a **required future gate**; no claim to recover safely from every mid-call module fault until a recovery contract exists and passes |
| M-D05 | Typed OOM rule vs infallible Rust allocation and current halt handler | Fallible-growth audit and reserve-backed failure reporting are prerequisites; do not replace halt with unsafe stack escape |
| M-D06 | New refcounted/drop-on-last image vs fixed eight never-evicted `Once` cache | Replace cache with facility-owned image/pin aliases; shared RO and private RW mapping, not relocation into another hardcoded slot |
| M-D07 | Module allocator forwarding vs current `drivers/vhost-filesystem` and `drivers/stuffit-filesystem` private global heaps (`mac-driver-alloc` calls the module global allocator) | New invocation adapter forwards explicit allocation context; migrate module implementations together, not by mixing heaps/free functions |
| M-D08 | Port/copy definition protocol vs current raw surface/DefinitionCall ABI | Introduce host-owned port operations and bounded copies/managed buffers; remove raw app pointer access before privilege gate |
| M-D09 | Synchronous participants vs earlier queued/deadline participant designs | **Ratified, Chloe 2026-10-23.** Synchronous participants stand; the earlier queued/deadline rule is superseded. Observers remain queued on their own execution context |
| M-D10 | Registered payload dispatch alone did not release empty per-owner value-array/cache backing | Closed for migrated stores: CSTA/SBST register backing lifetimes; the last child completes a deferred backing release, including after quarantine retry. The explicit control pruning hook is removed. Other manager saved-state slots and EXIT cleanup remain stage-2/3 migration work |
| M-D12 | Transfer child lifetimes and invocation/account attribution | High-level child exception removed by resource-scoped I/O: real subject owns fork/writer/group/metadata marks, root generation authorizes stopped-owner continuation, and child-first cleanup waits for the root. Lower driver request/job/device/module ownership and full invocation/account migration still gate complete owner-walk/accounting acceptance |
| M-D11 | Hard-bound capacity reasons are currently resident `&'static str` values, while module-sourced policies and empty pool history must survive provider unload | Required before module registration admission: retain kernel-owned bounded reason data in pool history. Never cast module text to a static reference, leak a per-registration string, or leave history pointing into unloaded code. Current resident literals are valid; this is module-phase migration work |

Chloe accepted M-D01, M-D03, M-D06, M-D07 and M-D08 as migration work as
classified above and authorized §13.2 staging on 2026-10-23. The remaining
profile/layout proposals are not blanket-ratified by accepting the structures.
Handle-ledger locking now uses the architecture invocation gate described above.
The physical buddy pool remains disconnected and its IRQ/preemption discipline
is still carried debt: close it **before it serves traps or interrupts**.
Host-thread stress alone cannot close that gate.

Execution identity has **four independent parts**: executor/fault domain,
subject process, allocation/zone account and stacked resource scope. A module
callback on an app control cannot conflate them. Current process arrays and
thread context are inspected migration inputs, not architecture to preserve.

`ExecutionBinding` has private fields and checked construction from the executor
and subject owner ledgers. Process execution cannot impersonate another subject;
the allocation account must belong to the subject. A control callback additionally
requires live instance and zone keys in that subject's ledger. The snapshot is
not authentication, a liveness pin or permission to dispatch after teardown:
the resident invocation guard must authenticate, revalidate and pin under its
admission lock before entering code or resource scopes.

Forwarding allocation returns the concrete, non-cloneable `ForwardingLease`,
which borrows its host allocator and owns the reserve/release pairing. Its Drop
passes the original allocation context, layout and storage back exactly once;
an implementation cannot substitute a lease with no destructor. Failed reserve
must undo its own provisional charge. Rust permits forgetting a lease, so the
zone ledger remains authoritative and must reclaim forgotten allocations at
instance teardown. These shapes do not yet install a live allocator.

## ISA-floor stage

The §13.2 first stage enables the complete compiler profile, with early platform
admission and FP/vector preservation in the RISC-V machine adapter. Register
state backing scales with discovered VLEN. Eager save is the correctness bridge
until per-thread domain backing/dirty policy lands; this is not the later lazy-
state or System-stack milestone. The LP64 transport ABI and development ABI 1
remain unchanged. Physical-pool IRQ-safe invocation is still a mandatory pre-use gate.
See [architecture](../ARCHITECTURE.md#native-isa-floor).

## Migration and gates

Reactor Room is now the first customer for stage-2 inspection APIs and a visible
acceptance surface for each later stage. Implement its application and native
data controls alongside the services, following [Reactor Room integration](REACTOR.md).
Enumeration, capabilities, revisions and cheap
cumulative counters are part of the model, not a private inspection backdoor.

Keep §13.2 stages explicit: ISA floor; facility/manager migration; owner walk and
unified rollback; dynamic process records; live page layer; heaps/reserve/control
zones; module domains/resource scopes/individual driver extraction/Host Clipboard;
TE2+Text Edit; P13 privilege campaign; **scheduler extension in its own isolated
commit after the full model boots cleanly**. No capstone scheduler feature slips
into a memory change.

Each manager migrates to the same facility before its new account is live.
The teardown algorithm has no manager-ordered phase list: stop admission and
threads, invalidate one ledger entry under lock, release lock, dispatch the
kind's registered cleanup (recursive release allowed), continue despite typed
cleanup failures, then wait for empty ledger and real thread/I/O quiescence.
A launch failure enters the same path. An incomplete drain quarantines storage;
it never reports success or reuses code/pages with outstanding users.

This first cut uses pure model tests for metadata/geometry and existing public
SDK tests for unchanged live behavior. Later stages add canonical tests through
`crates/mac-toolbox/tests`: stale/foreign handles and teardown, more than eight
apps, paging boot, per-domain exhaustion/retry, Res Edit full suite, 200 MB text
view, extension death/reconnect, privilege adversaries, then scheduler handoff.
Host-only tests cannot establish those System claims. Verification evidence for
this cut is recorded in the milestone report/status log and commit description.

## Cleanup completion and value-storage admission

ID invalidation is immediate, but it is not payload destruction. Each owner/kind
pool now has a bounded pending-cleanup bitmap beside occupancy/generations and
retirement. Invalidation removes the ID from the live set, advances/retires its
generation and produces one non-cloneable cleanup ticket. Pending slots cannot
be allocated. Only completing that exact ticket clears pending state; rejected
completion retains the ticket for a correct-owner retry. No callbacks run under
the owner-index lock.

Retirement checks live and pending counts atomically. A zero live bitmap with
unfinished cleanup cannot discard the owner record or claim zero leaks. Pending
counts cross the public inspection API and appear in Reactor Room's detail pane.
This is a per-resource completion barrier, not a global teardown phase or an
ordered manager cleanup list. It is required before resident value storage uses
the metadata slots, so a new generation cannot reveal a previous payload while
its disposal is still running. Actual kind registration/value-storage customers
and the common EXIT walk remain staged work.

Cleanup and invocation recovery apply to contained failures only, under M-D04.
A completed walk attempt can report quarantined resources; it cannot claim
complete reclamation or drop backing pages until pending cleanup and queues
are drained.

The walk provides resource containment. Arbitrary privileged-fault containment
is **unproven** and is a **required future gate**. Until a recovery contract
exists and passes, the System must not claim it can safely recover from every
mid-call module fault.


## First resident value-store customer: managed controls

`mac-object-table::store::Store` now pairs typed values with the owner's real
ledger. Managed controls are its first customer, including the boot Progress Bar.
The facility issues every native control identity; the manager no longer issues
IDs, stores a duplicate owner inside each payload, or shares a machine-wide
256-control generation budget. Control ownership remains a distinct policy check.
The System-domain bootstrap ledger is shared, not recreated for each panel.

Native IDs and domain incarnations share the boot-wide nonwrapping u64 issuer,
starting above the remaining u32 projection space. Tokens resolve through a
fallibly reserved hash index to full owner/kind/slot/generation keys. Values are
per owner, indexed directly by slot. Publication of ledger occupancy and its
payload happens inside one metadata operation after reserving backing storage;
stopping a domain cannot observe a live key with an unpublished value. Store
lookups validate the authoritative ledger every time. Growing prepared slots
starts at one and doubles as needed; it does not set a live-object limit.

A private, exclusive per-owner/kind claim prevents raw ledger allocation/growth/
completion from bypassing stored values. The claim stays alive through removal
guards that outlive the store. Invalidation comes first, payload destruction runs
outside the metadata lock, and the guard then acknowledges completed cleanup.
A forgotten guard or failed destructor leaves visible pending cleanup. A refused
walk dispatch retains its ticket. Empty value backing can be released on owner
exit while generation history stays in the ledger until domain retirement.

Canonical SDK tests inspect actual control births/deaths, force the sole slot's
reuse, reject stale and foreign IDs, exercise native/Pascal-string calls, and
create 300 simultaneous controls without the old global cap. Model tests cover
exclusive claims, a guard outliving its store, reentrant ledger reads during
destruction, failed cleanup, and publication racing domain stop. These are not
a claim that all managers or the generic release-dispatch registry have migrated:
the old EXIT cleanup roll call remains until the stage-3 walk replaces it.

Definition-facing follow-up is explicit: authors receive safe typed message/state/
port/service interfaces from one audited binding layer; unknown wire messages
fail before author code; the platform owns panic reporting. M-D04 still gates
arbitrary privileged-fault recovery. Static text must be transparent and all
new definition color work uses resident theme roles, not new literal palettes.

## Platform build and native-slice requirements

The accepted build contract is one command selecting a known platform and
producing deployable media, for example `cargo xtask image --platform <name>`.
Selection must encompass architecture/ISA, board facts, memory layout, drivers,
boot files and partition layout. Unknown or unsupported targets fail with an
informative error; a platform declaration alone is not a claim of hardware
support. A new board adds a definition and board adapter, not a fork of shared
kernel services. A new CPU family also needs its architecture backend.

The platform JSON is the home for defined layout policy. One checked record
emits both linker scripts/symbols and the Rust constants code consumes, plus the
BootInfo reservation list for the page layer. Sizes and ranges remain tunable.
Validate overlaps/alignment and statically knowable RAM bounds at build time;
validate resolved placement against discovered actual RAM at boot too. Current
QemuRISC-V now defines its physical reservations, flash, image/module placement,
relocation base, boot stack and current System heap in that record. The generated
ROM linker script and resident constants share it; those physical carveouts no
longer live as kernel-source address literals. Boot admission parses the DTB
before initializing its heap, reports all enabled RAM banks and combines firmware
reservations with platform exclusions. BootInfo transports those borrowed extents
and the loaded System reservation; System validates the same account at entry.
The summary RAM fields are derived from the primary bank and checked against it.

This is physical placement/boot transport groundwork, not live paging. The uniform
virtual layout, physmap mappings, buddy activation, later image allocations and
page-backed heaps remain their staged work. The ROM and System still execute in
M-mode with identity addressing; kernel self-virtualization needs the architecture
privilege transition in addition to SATP/table setup. No user-isolation claim
follows from the reservation list.

Physical-map qualification includes 21 parser/ABI/build host tests, 219 public
SDK host cases, optimized whole-world packaging, seventeen ROM boot/rejection/
fallback cases, actual 128 MiB/512 MiB/8 GiB boots and a pre-heap 64 MiB refusal.
Native/classic ECALL smoke, boot pixels, aggregate userspace and Reactor Room on
Linux/Windows also pass. A standalone `cargo xtask abi-check` runs the same Rust
smoke harness used by the canonical gate. Bootstrap library tests and the RAM
campaign now join that gate.

**Open performance qualification:** the strict 1-ms physical-delivery campaign is
not consistently met for the cold first key: a fixed three-run set measured
1.021, 0.978 and 1.478 ms. The queued-event maxima stayed below 0.597 ms. No
repaint or serial write appears in the first key's pre-routing interval; the
cause is still under investigation. Keep all samples, do not discard the cold
key or weaken the threshold, and do not call the latency baseline clean before
page activation. These emulated samples are not a hard real-time guarantee.
An event-queue tail-removal early-out now skips four empty array moves while
retaining live-index validation. Existing queue and public event contracts pass.
A subsequent fixed set measured first-key 1.010, 0.895 and 1.247 ms, so this
bounded improvement does not close the cold-input qualification.

Future Pi targets produce one whole-device image with a board boot partition
(firmware and loader) and an HFS-B System RISC partition. The loader discovers
the filesystem partition and blessed System Folder and selects native System
code. The platform build packages both partitions; manual assembly is not the
interface. Pi/ARM implementations remain future platform work.

Fat native bundles may carry RV64 and AR64 code slices, sharing non-code
resources. ABI/relocation metadata must be associated with the selected slice;
System's native handler makes that determination. Definition code needs the same
architecture selection while retaining semantic WDEF/CDEF/MDEF IDs. Today's
single-slice NABI/RLOC association is not yet that fat-bundle implementation.

## Enumeration requirements carried by the migration

The public inspection contract requires eight queries from the authoritative
facility:

1. Domains: kind, incarnation, human label, state and account summary.
2. Ledgers: kind counts, retired/pending counts, generation headroom and
   individual keys with owning registration and kind-specific identity labels.
   File entries must identify the item by volume and name/path.
3. Kind registrations: namespace, capacity policy, usage/quota and the owner
   of release dispatch.
4. Accounts: charges by category, quotas, System reserve and emergency-kit
   reserved/spent status.
5. Memory: buddy orders, System heap/reserve, domain ranges and control zones.
   At the page-layer stage include address-ordered regions with base, length,
   kind and charge; committed/used sub-extents; each named thread's stack.
6. Registrations/chains/images: order, readable provider names, owner domains,
   claimed evidence, revisions, image references and resident extents.
7. An ephemeral event journal: births/deaths, completed walks with leak counts,
   quota/allocation refusals and quarantine. Its default capacity is **10,000
   events**, configurable; no persistence.
8. Monotonic per-domain counters: CPU ticks attributed to the subject on whose
   behalf work ran, handle births/deaths, committed and charged totals.
   Mechanism accounting must not depend on scheduler policy.

Every enumeration states its snapshot generation and rejects stale continuation.
Refresh from changes/deltas or on demand; do not scan all objects on input events
or repaint. Until a source is live, mark its coverage unavailable rather than
inventing zero totals. Current process/control coverage is deliberately partial.
Registry existence/order/claims/mappings are ordinary public queries; inspector
authority gates cross-domain detail and termination, not registry existence.

Menu-bar extras, Get Info panels and language-pack contributions use the same
generation-scoped, ledger-owned registration mechanism. Extras publish state;
the Menu Manager renders only changed cells, coalesces updates and shows stale
owners. Teardown removes dead owners' extras. Get Info panels are additive,
match system-resolved item/folder/volume identities, and use safe port/services
within their section. Finder asks the System which panels apply. It does not
duplicate kind knowledge. These are registration customers, not a request to
implement the deferred Pipes/CLAP/Terminal/Shelly stack during this migration.

Localization is a resident public service: stable string IDs, named facts,
per-item language/dependency/base fallback, and language-pack grammar/formatters.
The base voice is Canadian English. Public collation supplies display ordering
and cached keys; a contracted custom sorter honestly lacks generated sort keys.
Language tailoring never changes HFS-B's fixed locale-independent name identity.
Language changes invalidate affected string/sort snapshots, not whole screens.
No new trust, suspend, outline-control or host-projection ruling is implied here.

### Per-owner pool census

Public `Snapshot::ledger` / `LedgerSnapshot::at` now read a domain's actual
prepared pools through the resident service. Each continuation checks membership,
the captured subject incarnation, and the ledger revision under its invocation
gate. Scope and inspector admission are checked again on every call; lending a
self-scope snapshot cannot relabel another caller's pools. Pool reads allocate
no resident cursor, copy no payloads and do not mutate revisions.

The census includes policy (with the reason for a hard bound), live, pending,
retired and prepared slots, cumulative births/deaths, and generation headroom.
Headroom is `u64::MAX - generation_high_water`: the high-water mark is maintained
on issuance/invalidation, including retired history. A zero value describes the
most advanced slot, not exhaustion of every other slot. Reads do not scan the
generation array. Empty pools retain their history until domain retirement.

Reactor Room shows this census below the selected domain summary through managed
cached rows. Selection/refresh/uncached scrolling obtains data; repaint never
enumerates. A stale snapshot refreshes on the next event and is labelled if churn
prevents completion. This is query 2's per-kind census, not completion of global
kind registrations, individual object labels, CPU/account/page accounting or the
generic release-dispatch walk. Those remain staged work.

## Registered cleanup and the first owner-walk customer

`mac-object-table::registry` now owns generation-tagged KREG entries in the
registrar's real ledger. KREG is the facility's explicit bootstrap namespace;
registered value stores receive a private validated `KindLease`, not a caller
assembled code/policy/dispatcher tuple. Each resource key also carries its
boot-unique registration binding. Equal FourCCs from another registry or a
replacement generation cannot consume each other's cleanup capabilities.

Registration metadata includes a readable kind/provider name, policy and source
evidence (resident or stable bundle/resource identity). Public object-kind
enumeration and ID lookup use this same registry; they do not require inspector
authority. Inspector grants still gate cross-domain ledger detail. Provider use
counts cover published resources, pending destruction and cleanup invocations.
Closing and incrementing admission share a checked atomic state transition.
Unregister stops new admission; busy providers stay owned and enumerable for
cleanup/retry. Successful unregister returns a RAII guard so provider/code-pin
destruction follows release of the host registry lock.

The host adapter implements `Release<Context, Failure>` at resource-cleanup
granularity. The failure carrier owns its unique ticket and typed manager cause;
I/O or driver errors need not become pool errors, statuses or guessed prose.
Invocation pins are obtained under the registry boundary and invoked after that
boundary is released. Loaded-module adapters must own their code/resource pins;
this landing supplies the resident Control Manager adapter, not module-domain
invocation recovery or the later module loader migration.

The generic owner walk stops admission, invalidates each resource first, then
resolves and calls its registered adapter. Parent cleanup can release children;
already-dead entries disappear naturally, including cycles. A contained refusal
transfers the ticket to preallocated quarantine bitmap storage in its real pool.
Reporting and retention allocate nothing. Pending/quarantined slots cannot be
reused or pass retirement. Retry is bounded to the initial quarantine count,
with rotating pool/slot cursors. Payload or cleanup panics never acknowledge the
owner barrier or drop the resource's provider-use pin. Host unwind tests establish
only that resource-lifetime property, not native privileged unwind.

Managed-control EXIT cleanup now uses this walk, and Reactor Room reads its
quarantine and named release-provider data through the public SDK. Stopping
domains stay inspectable but are no longer running applications; a retired app
cannot resume merely because cleanup returned an error. Managed controls and
scroll boxes now use `Store::owned`: their backing has a separate registered
CSTA/SBST lifetime. A retained empty cache is visible in the ledger; releasing
backing before its children defers completion until the last payload leaves.
Quarantined children retain that backing for retry. Deferred callbacks are
reported separately from completed releases; final pending counts govern
retirement. No Control/Scroll Box Manager finish-owner hook remains (M-D10).
Scroll boxes use full-width native IDs and fallible per-owner growth; the old
96-entry table is removed. Raster usage totals change with allocations rather
than being recomputed during presentation.

`Store::owned_state` also retains typed saved manager state under the backing's
registered lifetime. Its fallible constructor and destructor run outside owner
locks. Reads validate the backing/provider lifetime; an absent owner returns
without a provider lock. Failed state destruction keeps its backing pending and
provider pinned (host unwind evidence only, under M-D04).

Standard File uses this state for its active-dialog index, avoiding per-event
enumeration of completed dialogs. SFDL/SFST replace its old eight-entry table and
EXIT cleanup line, with native u64 identities and per-owner fallible growth. An
initialization guard owns the record through layout, window, font and drawing
setup; every early refusal closes its temporary windows and disposes the record.
Reserved request fields are validated before a Dispose can close anything.
QuickDraw offscreen worlds now use registered GWLD/GWST lifetimes and saved
per-owner pixel accounting. The old per-application counter and high-bit
target convention are removed; drawing destinations carry an explicit kind
and full-width identity. Window Manager now uses registered WIND/WNST lifetimes
and opaque full-width native IDs too; the old fixed 32-window table and 8-bit
generation limit are removed. Native process workers also leave
a world deliberately live so the registered exit walk must reclaim both world
and backing.

Window construction publishes its identity and payload together, then an
initialization guard owns WDEF New/Calculate and geometry validation. Ordinary
close and generic owner cleanup share invalidate-first release. A typed WDEF
Dispose refusal retains its payload, provider pin and unique quarantine ticket;
retry cannot revalidate the dead ID or touch a replacement. Parent modal and
Standard File protocols still dispose themselves and restore focus when a child
refuses cleanup. Native events and every window reference now carry u64 IDs
explicitly; the classic 16-byte codec stays separate. The trusted queue captures
close ownership before disposal, never from an application-supplied field.

Visibility sweep scratch grows fallibly at window creation, then paints without
allocation. Open/focus changes do not invalidate unrelated application contents;
close/hide/reorder damage only newly exposed content. Full-width canonical window
acceptance covers 40 simultaneous windows and 300 slot reuses. The process worker
leaves a window and its backing for the same generic walk as its other migrated
objects. Other process cleanup lines and hard limits remain staged work.

Window milestone qualification includes the optimized whole-world image, public
native conformance (40 simultaneous windows, 300 reuses, native/Pascal stale and
foreign rejection), per-kind process exit accounting, malformed-dialog retry,
original drag/grow pixels, physical tracking, retained updates and Reactor Room
on Linux/Windows QEMU. A worker's ten completed registered lifetimes require eight
walk callbacks because parent release also disposes children; diagnostics report
completed lifetimes separately. The strict 1-ms gate passed: decoded virtio input
to app delivery peaked at 0.964 ms and queued post-to-wait return at 0.460 ms under
contention. These samples exclude pre-driver/frame latency and are not a hard
real-time or MMU-isolation claim. Facility compile-fail tests also forbid extracting
non-Copy retained payloads through cleanup-state access.

Menus now follow that same registered ownership path. MENU owns the fallibly
allocated MDEF state; MNST owns ordering, selection and tracking for one actual
process incarnation. Current and saved application contexts dispatch release from
the ticket's authenticated owner; explicit disposal and process teardown share the
same release implementation. Installation reserves bar capacity before publishing
the payload. Disposal prepares fallible reflow before invalidation, so refusal
leaves the old menu intact. Empty/read/no-change requests do not allocate backing,
and hovering without a highlight change does not request recomposition.

Native MenuHandle is opaque u64; semantic i16 MenuId and the classic protocol stay
separate. The old global eight-menu table and generation array are removed.
Canonical public-SDK acceptance covers twelve simultaneous menus, sixty-four
reuses, stale/foreign rejection and ledger accounting. A native worker abandons a
menu plus its backing alongside the previous resources: twelve completed
lifetimes take ten walk callbacks because two children are disposed by parent
release. The optimized image, native original-menu comparisons, Linux/Windows
Reactor Room, aggregate conformance and strict latency gates pass. This remains
manager migration, not complete EXIT/rollback cleanup or live page activation.

Other managers, the rest of the old EXIT roll call, module-owned KREG teardown
and document aliases still require migration. Do not call this complete stage-3 cleanup or zero leaks across
all Toolbox kinds. An abandoned busy registry retains its provider/root pending
guard rather than falsely reclaiming it; controlled teardown keeps the registry
and retries, while the native resident registry lives for the System lifetime.

**M-D04:** The walk provides resource containment. Arbitrary privileged-fault
containment is **unproven** and is a **required future gate**. Until a recovery
contract exists and passes, the System must not claim it can safely recover from
every mid-call module fault.


### Input metadata qualification during manager migration

The native input path now normalizes key records once, retaining the original
raw OS queue before menu, modal and drag policy runs. Front-window coordinate
projection uses one bounded owner/opaque-ID memo; structural and visibility
changes invalidate it, and a cached live ID still passes the shared facility's
owner/lifetime validation. Bounds are read from the current payload. No event
query triggers drawing. The canonical public-SDK case exercises empty, create,
focus, close, replacement and explicitly stale target projections through real
services. Window visibility/group/domain changes have resident unit coverage.

Qualification: 56 facility tests (including three compile-fail checks), 131
System unit tests, 219 SDK host tests, optimized whole-world image, native
aggregate conformance, physical input/menu handling, original window tracking,
cursor tests and Reactor Room on Linux/Windows; boundary checks and 49-library
API documentation also pass. The strict latency gate remains **open**: the
three fixed measurements recorded decoded-key maxima of 1.2592, 1.6977 and
1.1905 ms, with queued maxima of 0.4379, 0.4370 and 0.5223 ms respectively.
All samples include the cold first key. These changes remove repeated metadata
work; the measurements do not establish a latency improvement or a hard
real-time guarantee. Evidence: private `input-metadata-*` reports. Paging is
still off, and the remaining manager/cleanup and dynamic-process stages precede
live MMU activation.


### Memory Manager ledger integration

MMBL registers every memory block, and MMST registers the per-domain arena,
classic-shaped projection table and MemErr state. Both use the shared store and
registry, including state created by an initial failing allocation or an error
query before any handle. Explicit disposal and generic teardown invalidate the
same canonical key; a pending backing remains until its last block is released.
A busy resident holder retains a typed cause and the unique cleanup ticket for
retry. Ordinary requests encountering a stopped backing return a typed refusal,
not a System panic. The old eight-element Memory Manager array, scheduler-slot
selector and memory-specific EXIT hook are removed.

The high16-generation/low16-one-based-slot u32 wire is unchanged. The shared
facility now retains projection generations across all owners for the whole
boot, preventing foreign-owner or recreated-zone aliasing. Exhausted slots retire;
the existing 512 projection slots, 4 MiB zone and 1 MiB block policies remain.
These policies are inherited limits, not a claim of page-backed heap accounting
or an unbounded native allocation API. Canonical retirement acceptance runs in
its own host process so deliberately exhausting the boot-wide issuer cannot
perturb other tests' slot-reuse setup.

Native threads bind the actual directory-issued OwnerRef; memory services do not
re-enter the Desktop lock or reconstruct authority from an application-supplied
number. Bootstrap/headless work uses the existing System owner. This bridge does
not implement module invocation's four-part identity, dynamic thread/image
capacity, process quiescence enforcement or MMU isolation; those remain staged.


Memory qualification: 58 facility tests (including three compile-fail checks),
131 System unit tests (one gated), 222 public SDK host tests (62 native gates),
optimized whole-world image, native aggregate conformance, both process campaigns,
physical events, Linux/Windows Reactor Room and headless native/classic ABI smoke
pass. A final review removed duplicate block validation; targeted host memory,
concurrency and original-oracle checks plus native aggregate/process/Windows/ABI
acceptance pass on that final image. Each native worker leaves MMBL=1 and MMST=1:
fourteen completed lifetimes require twelve walk callbacks, with no live/pending
resources. Boundary checks and all 49 library docs pass. Evidence is in the private
`memory-ledger-*` reports. This is not the entire model's acceptance campaign or
closure of the separately recorded cold first-key latency gate.


### TextEdit payload and saved-state migration

TEDT owns editor contents, layout and cursor bindings; TESV owns the per-domain
backing. A manager retains the actual directory OwnerRef through current/saved
context swaps. Registered release resolves the ticket's owner, invalidates the
same payload as explicit disposal and marks only cursor policy dirty when an
editor disappears. Cursor geometry updates use a live-only mutable store iterator
without allocating an ID list or holding an owner lock during mutation. No new
window repaint path is introduced.

Native EditorId is opaque u64. The old fixed-64 editor table and global u16
editor-generation array are removed. Counts grow until fallible storage refusal;
text contents still use the existing 32767-byte engine until stage-8 TE2 work.
Canonical acceptance now verifies 129 simultaneous editors, ledger-slot reuse,
full-width IDs, native/Pascal stale rejection, foreign-owner preservation,
stopped-owner refusal and cleanup quarantine/retry with diagnostic causes. Native
workers deliberately abandon an editor and backing as well: the expected complete
walk now has sixteen lifetimes and fourteen dispatcher calls. Qualification
passes: 59 facility tests (three compile-fail), four ABI foundation cases, 131
System units (one gated), 224 SDK host cases (62 native gates), optimized image,
native aggregate/process campaigns, original TextEdit pixels, real document and
Options interactions, typing/caret performance, cursor sprites/hover, Reactor
Room on Linux/Windows, native/classic headless ABI, boundaries and 49-library API
docs. No completed TE2 or page-backed allocation claim is made.

Two capture defects were corrected without changing their UI assertions: Options
resets only its offline private clone to application defaults instead of assuming
saved preferences are absent, and document navigation clicks the actual System
Folder icon after wide labels reserve extra cells. Original-image hashes remain
checked. Evidence, including initial capture failures and repaired runs, is in
the private `textedit-ledger-*` reports. The cold first-key strict-latency gate
remains separately open.


Stage-3 cleanup gate (updated by the retirement checkpoint slice below): EXIT
used to remove Saved contexts before checking domain retirement. Saved managers
now survive a refusal and outlive the retired thread's stack/context. One bounded
retry occurs at a genuine stack-release checkpoint. Controlled retry through the ordinary teardown
path, unified launch rollback, all file/provider lifetime registrations and the
complete I/O death barrier remain required. Passing normal native exit and host
provider-refusal tests does not establish those remaining resident failure paths.
This is separate from M-D04's privileged-fault containment gate.


### Classic Alert protocol and managed-button migration

ALRT registers one classic Alert protocol per actual app owner; ALST owns its
saved backing. This preserves the existing one-active-Alert rule. The public
window reference remains an opaque WindowId; the protocol validates that full
live identity on every request. Closing the window, replacing it with a crash
report, explicit Alert disposal and the generic walk all invalidate the same
protocol. Window and Alert cleanup can occur in either order without reviving
modal state or foregrounding a saved app. An initialization guard owns rollback
before creating a window or button. Child Window Manager cleanup refusals retain
their own tickets after the parent protocol is gone.

DITL buttons now create real managed CDEF controls. Their default ring belongs
to the control bounds; the parser checks ring expansion and reserved fields
before publication. Highlight, drag-out, reentry and cancellation use the Control
Manager tracking contract. A changed button replaces only its own retained draw;
HANDLE_EVENT no longer erases/redraws the Alert window or clones all item strings.
Unchanged held-pointer tracking performs no recomposition or menu painting.

Qualification passes: 59 shared-facility tests (including three compile-fail),
131 System units (one separately gated), 225 SDK host cases (63 native gates),
optimized whole-world image, native aggregate, real managed-Alert tracking with
pixel/frame-copy bounds and no-change counters, both process campaigns, original
Finder Trash dialog pixels and modal input across three boots, panic diagnostics
and recovery, physical event routing, Reactor Room on Linux/Windows, native and
classic ABI smoke, boundaries and 49-library API docs. Each worker abandons
21 registered lifetimes, completed through 16 callbacks with zero live/pending.

Verification also repaired capture assumptions without changing UI or pixel
acceptance: Trash clicks follow the fourth icon onto row two after wide-caption
layout; panic relaunch uses actual retirement records rather than decoding bits
of an opaque process incarnation. The Memory reuse test requires generation
advancement, allowing intervening generations consumed by concurrent domains.
Initial failures and final results remain in private `dialog-ledger-*` evidence.
The current stage-3 Saved-context retention/retry debt and cold first-key latency
gate remain open; paging, dynamic thread/image capacity and std support are not
claimed by this manager slice.


### Drag session/receiver migration and leased-response expiry

DRAG/DRST register session payloads and backing; DRCV/DRVS register receivers and
backing, including each window's receiver. The actual directory OwnerRef admits
both kinds. The old drag-specific EXIT call is removed. Kind callbacks release
source/receiver state in either order, wake surviving peers, and remove only the
old outline/highlight feedback. Source death wakes a pending promised-data take;
receiver death preserves the cancellation cause and rejects the drop. Stale IDs
are already inaccessible while a refused cleanup ticket remains quarantined.

The narrow, safe shared `mac-object-table::projected::Store` places existing
32-bit treaty namespaces over the registered value store. It is an index, not
another payload or owner authority. Admission preflights the projection and rolls
it back if registered publication refuses; cleanup removes both indexes while a
linear guard retains payload/backing/provider pins. Boot-wide projection history
prevents aliasing across manager reconstruction. Construction refuses an
unmigrated namespace without that history. Current DragRef wire limits remain
32 sessions and 64 receivers globally, derived from the central namespace record.
This slice does not introduce original callback execution or expand that wire.

Native workflow qualification exposed a pre-existing delayed-tracking-response
race: responding after a committed drop returned ACCESS_DENIED; Finder's error
guard then abandoned the valid source. A deterministic SDK regression reproduced
it. ACCEPT now returns Applied/Expired atomically with validation, without an
extra query trap. Finder stages a proposed target/document batch and publishes
it only on Applied, retaining the last valid pins across expiry. An older point
cannot withdraw a newer highlight. Genuine foreign/thread/malformed refusals
remain errors; DragError now exposes informative Display and nested promise
causes. Original Pascal transport/layouts are unchanged.

Qualification passes: 63 facility tests (including three compile-fail), 131 System
units (one separately gated), 232 SDK host cases (63 native gates), optimized
whole-world build, native aggregate with 64 native/classic projection reuses,
both process campaigns (27 completed lifetimes through 19 callbacks per worker),
Finder failed-drop recovery/group placement and external file/app/document/eject
workflows. A fixed three-run post-fix external workflow set passed every sample.
Tracking preserves zero file/font calls and unchanged scene/menu/update counters.
Cursor, Alert bounded painting/no-change tracking, panic recovery, physical events,
Linux/Windows Reactor Room, native/classic ABI, boundaries and 49-library API docs
also pass. Initial failures and final evidence are retained as `drag-ledger-*`.

The captures also now target the actual second-row conflict folder, and the
placement assertion accounts for Reactor Room and wide-caption rows. The native
worker uses the public facade's ABI re-export. These repairs retain all existing
pixel, no-enumeration, bounded-paint, file-identity and persistent-placement
assertions. Other file/resource/module lifetimes and the Saved-context retry /
quiescence gate still need migration. Paging, dynamic scheduler/image capacity,
privileged-fault containment, strict cold-key latency and std readiness remain
unqualified by this slice.

Final error-identity review separates explicit response expiry from target
admission failure. The resident replies with a typed wire context and unchanged
Status/cause; Rust exposes TargetUnavailable with an Error source. An expired
view skips the File Manager predicate entirely, while a genuinely missing target
rejects through RAII and leaves the next drag usable. Canonical tests also walk
an active same-owner drag in both registration orders, during tracking and drop,
then drop its stale source guard after creating a replacement.


## Retirement checkpoints retain saved managers

EXIT first closes ledger admission and retires the current application thread.
The common invalidate-first walk then releases registered objects, preserving
any refused tickets. EXIT parks the app-local managers in Saved and removes the
app from ordinary running/application-menu views; it does not discard Saved or
retire the process directory entry on the dying stack.

The machine adapter counts retired-but-not-reclaimed thread records as users.
Only a trap executing on another stack destroys their contexts/stacks. A real
stack release schedules one resident reap checkpoint outside the scheduler lock;
registered async-resource completion now schedules the same checkpoint too.
Unchanged timer/input events neither scan dying domains nor retry callbacks;
thread-stack scanning itself early-outs unless a retirement is pending. At the
checkpoint, each stopping domain without a thread gets at most its initial
quarantine count of retries. Failure retains its directory entry, saved managers,
provider pins and typed cleanup diagnostics. Successful directory retirement
precedes Saved destruction. The idle/boot case explicitly parks any dying active
manager set in its saved entry before dropping that entry.

Relaunch refuses a file whose previous process is still stopping, so it cannot
reload that process's writable image under retained callbacks. Post-thread
cleanup gets an opaque resource binding minted from the actual directory entry,
including its ledger pin and image index. Synchronous RAII scopes restore the
previous resource context; unknown owners fail instead of inheriting Finder's
resources. This is resource-scope retention, not the future full module-domain
invocation or arbitrary privileged-fault recovery contract.

Coverage is deliberately split: canonical public SDK inspection tests exercise
real quarantine, retirement refusal with preserved diagnosis, cleanup retry and
successful removal; native process campaigns assert stopped -> owner walk ->
different-stack reclaim -> final retirement for every leaked-resource worker.
This does not inject a privileged fault or establish every Saved-manager failure
path in a running guest. File/document/provider lifetime registration, complete
I/O quiescence, controlled retry through ordinary teardown, unified launch rollback and
Finder restart remain stage-3 work. Physical images currently stay resident;
this checkpoint does not authorize unmapping pages or unloading their code.


## Directory watches join registered file lifetimes

FWCH/FWST register directory watches and their per-domain backing with the
System File Manager. The checked native/classic file boundary captures the real
OwnerRef before the bootstrap I/O handoff; a scalar process number cannot create
one of these objects. Provider-read-only watches remain valid no-op monitors.
The old watch-specific process-exit hook is removed. Other file marks, transfers,
document pins and volume/module lifetimes remain staged migrations; partial
ledger coverage continues to say so.

The shared projected store reserves both the native payload and its existing
32-bit treaty slot until the linear removal guard is destroyed. Its projection
index is shared with guards through an architecture-gated lock; payload cleanup
never runs under that lock. This preserves the wire layout and generation
history, including stale/foreign rejection, without acknowledging async work
when an ordinary ID is invalidated.

A provider probe carries a private owner/volume/directory/state lease. Its
state is allocated once per monitored watch and reused; immutable/block-only
watches need no probe allocation. A terminal ACK ends the provider read lease
before releasing a retained watch guard. If metadata invalidation preceded a
refused cleanup callback, ACK marks the retained state complete so the callback
can finish on retry without resolving the invalid ordinary ID. Only that actual
cleanup change schedules process reaping. File callbacks still run outside the
owner-index lock.

Probe-fault capacity is reserved before operation publication. An uncertain
job or refused ACK retains its owned data/lease and quarantines its mount in a
separate queue, allowing other mounts to progress. Lease completion preflights
fallible work before mutation; malformed terminal data is a watch error, while
uncertain completion never recycles a lifetime. Idle pumping early-outs before
its locks until a real active probe or the next monitor deadline exists.

Canonical public SDK cases cover a full projection namespace with pending
disposal, forced reuse only after ACK, late ACK before/after failed provider
resolution, stopped-owner refusal and transport-unwind RAII. The host fixture
supplies transport acknowledgement; it does not prove privileged panic recovery
under M-D04. Native conformance adds 64 reuse cycles through native and classic
traps; process workers abandon FWCH/FWST with the other registered resources.
Verification results belong in the milestone report after checks finish.

### Boot-wide file treaty generation history

All central treaty namespaces now use boot-wide generation history, including
open forks, staged writes and mounts. Clearing, reconstructing or concurrently
instantiating a File Manager cannot mint another manager's live or stale ID.
The high16-generation/low16-slot contract and separate owner checks are unchanged;
exhausted generations retire a slot rather than wrapping. Namespace history
selection is exhaustive, with no per-table fallback. Registered projections no
longer need a temporary exclusion for not-yet-migrated namespaces.

Canonical public SDK checks recreate the real disk service with the same caller,
force the same volume/fork/write slots, reject prior IDs through native and
classic gates, and verify current objects remain usable. This is identity
foundation for the remaining File Manager registration work, not a claim that
its forks, transactions, mount references or I/O death barrier have migrated.

### Resident panic discipline and failure checkpoints

Chloe's 2026-10-24 ruling is recorded in AGENTS.md: cleanup, reap and paint
paths cannot use `.expect` or substitute another panic for a recoverable
refusal. Resident access can transfer an owned argument only after its owner,
state and lock checks pass; refusal returns the original cleanup capability.
Window disposal resolves the retained payload fallibly. Ordinary menu and
Standard File disposal take registered removal guards before unpublishing their
state or disposing children, retaining backing until those guards leave scope.

A process preallocates a shared cleanup checkpoint. If a walk cannot quarantine
its failed ticket, that checkpoint retains the typed error and capability; final
process retirement refuses while the checkpoint is running, retained or did not
return. A later ordinary reclamation checkpoint first restores the ticket to
quarantine and retries the existing walk. The attempt is linear and its private
state lock is released before any cleanup callback. Window initialization and
explicit disposal also reserve failure storage before publication; an unexpected
quarantine refusal retains its ticket/backing rather than killing the machine.
Those retained window refusals are retried at actual create/cleanup checkpoints,
not on physical input or timer ticks. No separate inspector action is added.

The walk provides resource containment. Arbitrary privileged-fault containment
is **unproven** and is a **required future gate**. Until a recovery contract
exists and passes, the System must not claim it can safely recover from every
mid-call module fault. The host transport unwind test leaves a checkpoint
interrupted; it is not evidence of native privileged-fault recovery.

Content-cache replay propagates a missing resource context without marking
partial pixels valid. A refused scene keeps its exact pending damage and skips
presentation; cursor-only copies cannot leak an incomplete private back buffer.
Successful replay retains the existing clipped/cached path. Directory-probe ACK
holds its checked mutable mount borrow through read-lease completion and revision
publication, without a second preflight-and-expect lookup.

Drag notifications use an owned queue whose bounded per-operation effect budget
is reserved fallibly before protocol mutation and held through use by the same
exclusive manager borrow. Cleanup reservation failure returns the unique ticket
and an informative typed cause into ordinary quarantine. Input/cleanup callers
drain already-queued effects even when an operation refuses. Stale session reads
are fallible; notification production no longer asserts a fixed empty slot.
Idle animation and unchanged pointer motion early-out before reserving effects.
The canonical SDK fault case covers refused cancellation without state change,
failed teardown with a retained ledger barrier, ordinary retry and a usable next
gesture. Full paging/I/O/model and strict physical-input gates remain separate.

Native Finder qualification exposed deferred pointer events being replayed
inside a browser window, bypassing the application's Drag Toolbox dispatch.
Ready cached events now re-enter that same dispatch in order. A matching
physical press/release observation preserves the actual drop position and
release-time modifiers when BEGIN is delayed; a disposed receiver cannot be
replaced by a newly registered receiver. Repeated release is a no-op. The
public SDK tests cover cancellation refusal, retained cleanup/retry, delayed
release and receiver replacement; physical Finder campaigns cover selection,
group drag, external open/copy/move and unmount workflows.

The deferred-close acceptance test also uses the public owner ledger to prove
storage reuse and stale-window rejection, rather than decoding opaque ID bits.
These checks do not certify image unloading or arbitrary privileged unwind:
failed replay is fault-injected at the cache boundary, and definition panic
continues to report its authenticated resource identity and shut down loudly.


## Open forks join registered file lifetimes

FFRK and FFST register open-fork payloads and per-owner backing with the same
File Manager release dispatcher as explicit close. Both data and resource forks
retain the existing 32-bit projection and boot-wide generation history. Native
and Pascal entry paths capture the genuine owner authority before firmware I/O
switches to bootstrap context; resident background coordinators resolve their
trusted subject before taking the Files guard. A scalar alone cannot admit an
application's fork. System-owned loader/source marks use the System ledger. Transitional transfer
coordinators now use the resource scopes described below, replacing the earlier
System-backed internal fork exception. Lower-layer attribution remains M-D12
migration debt until transfers join registered ownership.

The common owner walk invalidates a fork before dispatch. Block-backed marks
release immediately. A provider mark keeps its unique removal guard, projection
slot, owner authority and catalog/volume protection until outstanding reads and
Close acknowledge. Opaque facility-issued completion keys retain read authority for already-issued
operations, including when cleanup was quarantined before dispatch. They cannot
be constructed from a wire ID, do not revive it, and do not acknowledge cleanup.
An early terminal Close is recorded and the real cleanup ticket still owns final
destruction on retry. Only resident completion paths can reach that retained state;
ordinary native/classic calls reject its old ID. Completion schedules the existing
retirement checkpoint, so Saved state cannot disappear while a fork is pending.
Cleanup retention capacity is reserved before publication; refusal preserves its
unique ticket for quarantine/retry instead of panicking. Pending fork cleanup
also refuses normal power requests.

The fork-specific loop is removed from the transitional file EXIT helper.
Remaining file writes, streams, groups, transfers, document/executable aliases,
volume/module lifetimes and full provider-failure recovery still require their
own registered integration. This slice does not complete stage 2 or stage 3,
remove the existing file projection capacity, activate paging, or make application
std available. Reactor Room displays the real FFRK/FFST pool counts; individual
file identity details remain part of the public enumeration work.


## Staged data writes join registered file lifetimes

FDWR and FDST register staged data-file writes, provider metadata transactions
and their per-owner backing. The same safe file-object store now serves forks
and writes, forwarding identity, projection history and completion authority to
the shared facility. Payload types fix their namespace and registered kind;
preparation rejects a mismatched kind lease. A generic file caller carries the
genuine owner authority through native and Pascal entry before firmware handoff.

RAM-only drafts release without storage side effects. Submitted/uncertain provider
transactions keep their token, payload and catalog/volume protection through
terminal publication or rollback acknowledgement. Late callbacks use the opaque
completion key through quarantine. Recorded terminal state makes later release
retry a disposal rather than a second Abort. Retention is reserved before
publication; an admission/cleanup refusal retains typed owner/kind/capacity facts
and the unique cleanup ticket. The old data-write EXIT loop is removed.

A local commit now retains its staging record on ordinary refusal or a durable
rollback. `DataWriter::finish(&mut self)` can therefore retry or abort the same
handle rather than discovering that System discarded it after returning an
error. Protected-ID storage and the catalog/watch revision are prepared before
I/O; successful publication consumes the record and publishes that reserved
revision once. Failed rollback still poisons the volume and is not reported as
successful recovery.

The later resource-scoped transfer migration removes the high-level System exception; lower-layer attribution remains staged under M-D12
exception until transfers themselves migrate. Icon groups,
transfers, document/image/volume/module lifetimes and full I/O quiescence remain
unfinished; this is not full stage-2/3 cleanup or page/heap/std activation.

## Streamed fork writers retain storage through acknowledged cleanup

FSWR and FSST register streamed two-fork writes and their per-owner value
storage through the same File Manager lifetime facility. Native admission
captures the real owner before firmware stack handoff. Drop/ABANDON invalidates
the ordinary projection immediately; the same projection generation stays
reserved by a retained guard until storage confirms the final cleanup step.
Explicit abort remains a pollable live operation until its terminal reply.

The common owner walk schedules native bounded rollback or provider AbortWrite.
It does not free a provisional StagedFile, provider token or process charge just
because the ordinary handle is invalid. Internal progress lives behind a private
lock; only opaque, pre-invalidation completion authority can reach a pending
object. A terminal reply records Closed before disposal, including during
pre-dispatch quarantine, so later cleanup does not repeat an acknowledged Abort.
Unmount, conflicting mutations and normal power remain blocked while any such
payload is retained. A cleanup failure keeps its payload owned and reports the
existing rollback failure; it cannot manufacture a successful retirement.

The streamed-write scan is removed from the transitional EXIT helper. Native
publication reserves its change revision before writing and reuses it for
catalog/watch publication. Public SDK conformance covers stale/foreign IDs,
immediate abandonment invalidation, forced slot reuse only after real rollback,
owner-walk refusal/retry and another writer continuing independently. Provider
unit checks also cover a late reply before cleanup dispatch. This does not yet
complete icon-group, transfer, mount or module lifetime migration; the M-D12
coordinator exception still applies until registered transfers replace it.

## Icon-move groups use the registered file-object walk

FIGR and FIGS register icon-move payloads and per-owner backing. Admission
captures the actual owner before firmware I/O; native and provider paths use
the same checked projection and authority. The explicit Drop/ABANDON path
invalidates first. Native groups have no filesystem side effects before their
atomic step and can release unapplied rows immediately. Provider groups retain
rows, affected parents, tokens and charges until a validated terminal apply/undo
reply. Native rollback failures still retain quarantine state; this does not
claim that a poisoned volume has been repaired.

Private progress is synchronized and late callbacks use pre-invalidation
completion authority. A terminal acknowledgement records Closed before the
retained guard is dropped, including when cleanup dispatch was refused first.
Notifications can inspect the retained parent set without reviving the ordinary
ID. Transfer error handling checks retained group state rather than treating an
invalid public ID as a rollback acknowledgement. Unknown provider operations
are rejected before admission or setting the running flag.

The icon-group and transfer EXIT loops are removed. Executable pins now use
registered shared aliases; document delivery now uses its own registered batch/scoped-alias lifetime. Canonical public SDK tests cover foreign/stale/reused IDs,
no-change abandonment, real guard Drop, owner-walk refusal/retry and a fresh
group committing after cleanup. Native process workers deliberately leave an
actual group and its backing among their 35 registered resources. Complete
remaining alias and I/O quiescence gates remain open, including M-D12.

## Transfer coordinators hold the process I/O death barrier

FTXN and FTXS register recursive transfer controllers and per-owner backing.
Admission captures the real owner before taking File Manager or firmware state.
Dropping the public owner invalidates its ID immediately; an internal completion
capability retains the controller, undo receipts, child I/O state and charges
until the actual terminal result. Cancel remains a pollable live operation.
An invalid public ID is never used as evidence that rollback finished.

Controller state is synchronized privately. The bounded pump borrows no Files,
Desktop or controller lock over a provider callback/wait. Restoration observes
late cancellation before the commit barrier and retains the existing bounded
cleanup retry policy. Owner-walk disposal holds the same controller as normal
Drop; the transfer-specific EXIT cancellation scan is removed. Blocking classic
file operations retain their registered bridge through cancellation and replay.

The later resource-scoped child-I/O migration removes this coordinator slice's
private numeric tag and System-backed child exception. Lower driver ownership,
account/invocation authority and remaining shared aliases still gate complete
quiescence and charging.

The growing resident image also exhausted its existing physical reservation.
Window grow-tracking scratch now occupies a dedicated page-aligned platform RAM
range outside image BSS; the checked record emits its linker/Rust constants and
BootInfo reservation. Bootstrap uses it for its framebuffer; the resident
tracker can borrow it only after System successfully installs a different RAMFB.
Boot resident data/stack and scratch are disjoint checked ranges. The full module
arena, minimum RAM, app image ranges and current 8 MiB heap stay unchanged. Each
serialized display boundary initializes bytes before publishing its reference;
resident paints then reuse the pinned storage without allocation. BootInfo carries
an explicit scratch loan, validated against the selected reserved range before
heap use; a stale header or mismatched/unreserved loan is rejected with rebuild
instructions. The development ABI remains version 1. This remains a direct identity-mapped device
boundary until the page layer supplies its supervisor mapping.


## Executable aliases and launch-failure cleanup

`mac-object-table::aliases` owns both sides of a shared reference: a wide native
System target and a separately registered process alias. Both namespaces and
backing stores are registry capabilities, with configurable capacity policy.
Targets are immutable; resolution checks the complete alias owner/generation and
the target generation on each use. A resolved borrow cannot outlive that
validation without keeping the store borrowed. Sharing requires genuine source
and recipient ledgers and publishes a distinct recipient alias. Refused sharing
leaves the source and reference count unchanged. A caller cannot construct a
target key, reassign an existing alias's owner or supply cleanup authority by ID.

The File Manager's executable pins are this facility's first resident consumer:
FPIN/FPST belong to the process, FPOB/FPOS to System. Equal catalog references
share a target. Removing the last alias drops that target; invalidating an alias
alone does not. Mutation and unmount protection enumerate retained System targets,
including those whose aliases await a refused cleanup dispatcher. If System
invalidates a target first, further ordinary resolution fails and target cleanup
retains its unique ticket until every alias is released. Store destruction also
retains targets referenced by a pending dispatcher instead of pretending that
abandonment acknowledged cleanup. No payload destructor runs under a ledger lock.

Executable teardown no longer needs a File Manager EXIT hook. Pre-thread launch
failures stop admission and use the same owner walk as EXIT, keeping saved manager
state reachable until its barrier closes. The cleanup resource binding is minted
from the subject's directory entry; cleanup never borrows the launcher's image as
its subject. A failure before the walk can enter leaves live resources for the
ordinary retirement checkpoint to drain. No per-input retries or new fixed
capacity are introduced.

Immutable image ownership, volume/device/module aliases,
M-D12 lower-driver attribution and the full owner-walk/launch/recovery gate remain
open. Ledger lifetimes are not byte accounting: attribution, quotas against
committed pages and actual heap/page reclamation still need the later allocator
and paging stages. This slice does not enable the approved heap defaults, MMU,
process isolation, multihart scheduling or userspace std.

Qualification: 68 shared-facility cases (including compile-fail), 154 System
cases and 27 scoped public-SDK cases pass, with one separate System gate. The
alias test forces a one-slot reuse, rejects stale/foreign IDs, refuses stopped
or over-quota recipients without extra targets, and retains a target's unique
ticket when System invalidates before its aliases. Canonical HFS-B acceptance
keeps mutation protection across independent owner exits and a quarantined
last-alias dispatcher, then permits deletion after the actual retry. Six native
checks pass: both 37-resource process campaigns, full provider workflow,
aggregate Toolbox, bounded Finder selection/drag and original-sprite cursors.
Native/classic ABI smoke, boundaries, formatting and 52-library API generation
pass; tested packaged image hashes remain unchanged through ABI packaging.
These checks do not yet exercise injected failure of native thread creation.


## Document batches and scoped catalog aliases

Document delivery now uses registered DBAT/DBST native storage with opaque u64
IDs and grow-until-memory policy. The native-only sixteen-batch projection is
removed. Neither a batch nor a file alias can change its authenticated owner.
Commit clones the bounded delivery snapshot into a new receiver-owned batch,
shares catalog aliases to the receiver, publishes its notification, and releases
the sender. Refusal leaves the source set intact and schedules cleanup of any
unpublished receiver state. The native event decoder preserves the full ID.
The development ABI remains 1 and all producers/consumers rebuild together;
the classic treaty and its other projection namespaces are unchanged.

Executable and document references share the catalog-alias store. A document
batch owns a non-cloneable scope; its pin aliases hold read-only scope leases.
The owner walk can visit aliases before their batch's I/O finishes: preallocated
native removal guards retain those aliases, their target references and the
process barrier until the actual scope closes. This is successful deferral, not
a quarantine error. The real File service admits unpinning only after matching
lower I/O has drained. Closing the scope releases ready guards; backing and
process retirement still follow their own unique tickets. Ordinary document
Drop/Abort invalidates immediately while cleanup remains retained.

The document-specific EXIT scan and hand-maintained pin list are deleted.
Cancellation and lower-flight terminal completion publish a cleanup revision;
a bounded pass visits each waiting batch once for that change. No document scan
runs on physical input while waiting for I/O. Normal live-owner completion does
not wake the process reaper. Bulk alias removal makes one metadata pass and
stopping owners refuse new admission before allocation or value construction.

Image, volume/device/module/resource aliases, M-D12 lower-driver attribution,
full I/O-scope integration and launch/Finder recovery remain staged. Byte
accounting, dynamic process/thread/image capacity, live pages/MMU and userspace
std are not enabled by this document migration.

Qualification: 255 scoped host cases pass (one separate System gate), including
wide request layout, shared-facility deferral/quota/reuse and canonical SDK
protection through a held asynchronous scope. Nine native cases pass: both
process campaigns, provider workflow, real external Finder document drops,
Text Edit preference documents/open/save, both disconnected endpoints,
aggregate Toolbox, bounded Finder selection/drag and original-sprite cursors.
Native actors hold 20 batches, exercise stale/foreign/unsupported/duplicate
requests with readable diagnostics, check the full-width typed notification,
and return with a real receiving batch and three scoped pins pending. Rust
asserts their four-resource barrier and stack-reclaim/retirement ordering;
ordinary process campaigns retain their 37-resource census. Revisioned
inspection retries reacquire snapshots after typed stale-read refusals.
Native/classic ABI, boundaries, formatting and 52-library API generation pass;
tested packaged image hashes remain unchanged through ABI packaging. Delayed
document I/O is held by the trusted host scope fixture; native process exit
exercises actual batch/pin deferral, and provider tests exercise real I/O
separately. This is not the full child-I/O attribution or fault-containment gate.


## Retained cleanup completion

Object and per-owner backing stores reserve a cleanup record before publication.
Payload destruction runs outside the ledger lock with that record already held.
Successful destruction acknowledges its unique ticket and releases the registered
provider pin. A refused acknowledgement retains the ticket, pin and typed cause
in the authoritative owner ledger; a later cleanup checkpoint retries only that
acknowledgement. Destructors are never retried. No failure-path allocation or
cleanup assertion is required. Treaty projection slots are released only after
successful native completion.

If another dispatcher already owns a backing ticket, destruction retains a
waiting record until that exact ticket returns through the walk. Waiting is
successful deferral. An interrupted destructor retains its capability and pin,
blocks domain retirement and provider unloading, and reports the recovery gate.
Arbitrary privileged-fault containment is unproven (M-D04), a required future gate.
Host unwinding exercises retention; it does not prove native privileged recovery.
The native panic path remains a loud System failure, not a silent hang or a
promise of arbitrary mid-call recovery.

The public inspection SDK exposes completion counts and revision-checked rows
with the full resource identity, provider registration, typed reason and bounded
UTF-8 diagnostic with an explicit truncation flag. These records are already
included in pending counts; they are not extra resources or fictional successful
releases. Reactor Room reads this ordinary inspection surface. Own-domain access
is public; cross-domain detail keeps the existing policy check. Enumeration and
the retirement barrier read the same ledger, with no hidden inspector list.

The reservations remain metadata allocations under the current allocator. Their
future zone/page charging, paging, physical reclamation, MMU and std gates are
unchanged; this lifetime discipline does not enable the approved heap budgets.

Qualification: 259 scoped host cases pass, with one separate System gate.
Shared-facility fault injection proves retained refused acknowledgements,
provider pins, exact treaty-slot release on retry, stale-ID rejection and
no repeated destruction. Canonical SDK tests inspect real object/backing
interruption, reject foreign/stale reads and exercise an independent working
control after the failure. Seven native cases pass: both process campaigns,
provider workflow, aggregate Toolbox, bounded Finder selection, original-sprite
cursors and Reactor Room. Native cleanup checks assert no retained completion
faults during ordinary lifecycles; native privileged fault recovery is not
injected or claimed. Native/classic ABI, boundaries, formatting and 52-library
API generation pass. Packaged artifact hashes stay unchanged throughout final
acceptance. This is a shared cleanup milestone, not completion of the remaining
resource-scope, accounting, page-layer or privilege stages.

## Resource-scoped child I/O

An unpublished scope reservation carries a genuine owner and intended root kind.
The matching native store binds its full resource key atomically with payload
publication, and retains the unique RAII close owner. Authors hold read-only
leases, which cannot publish, reopen or upgrade a scope into general OwnerRef
authority. Construction errors close unpublished reservations without exposing
children. Root destruction closes the scope after payload cleanup; an interrupted
privileged destructor remains subject to M-D04.

Scoped child admission validates the root's owner, kind, registration and complete
generation under the same owner metadata gate as closure and publication. A
live or pending root may finish its existing operation after ordinary admission
stops. This never reopens ordinary admission, bypasses quota or ignores a closing
provider registration. A closed/unpublished/foreign scope is refused with a typed
cause before child publication. Per-slot parent identities remain until child
cleanup completes. Public pool/domain scope counts include live and pending
resources, including owner backing first admitted by an operation; they count
creation attribution, not currently open scopes. Reactor Room reads those counts
through the ordinary revisioned SDK surface.

Transfers no longer authorize work with a numeric pseudo-owner or a System-owner
exception. File background work, asynchronous replies and delayed cleanup retain
the actual subject and operation capability. Forks, fork writers, icon groups and
metadata writes publish in that subject's ledger. Ordinary requests cannot access
an operation's private handle even for the same app. A root uses authenticated
retained completion authority after owner-walk invalidation; fork cache and group
rows remain protected state without a mutable removal-guard escape.

If the walk reaches a child first, its preallocated removal guard waits for the
actual root instead of prematurely abandoning the result needed by rollback.
Explicit operation cleanup still consumes real lower acknowledgements. Closing
a root triggers bounded per-kind cleanup through a change clock; an unchanged
clock performs no object scan. Each published scope consumes a never-repeated
native identity and closes once, so the identity space is exhausted before the
close clock could wrap. Scope closure never synthesizes device completion.

The high-level transfer tag/System-lifetime exception is removed. Lower driver
request/job/device/module ledger migration, full invocation/account attribution,
remaining shared aliases and the complete launch/recovery campaign still gate
completion of the model. The later native file-identity migration removes
native-only fixed file-protocol projections; the classic treaty remains supported. This slice
does not enable byte budgets, physical reclamation, MMU, multihart or std.

Qualification: 264 scoped host cases pass (one separate System gate), including
publication/generation/quota/foreign scope cases and canonical SDK child-first
walk, issued read after stop, ordinary stale/private-handle refusal, independent
working I/O and final retirement. Eight native cases pass: both process campaigns,
full provider copy/exit/rollback/retry, endpoint disconnections, aggregate Toolbox,
bounded Finder updates, original-sprite cursors and Reactor Room. Real-byte copy
exit asserts app-owned fork/writer scopes and a multi-resource pending barrier.
Native/classic ABI, boundaries, formatting and 52-library API generation pass;
final packaged hashes remain unchanged. The host lifetime transport controls
root completion; native assertions exercise actual provider I/O separately.
Arbitrary privileged-fault recovery and the remaining model stages are not
claimed by these checks.


### Native file-operation identities

Staged data writes, streamed writes, icon groups and transfer coordinators use
full-width shared-facility native IDs and grow-until-memory policy. Their former
8/8/8/4 projection namespaces, generation arrays and bit-range dispatch are
removed. Genuine classic open-file refnums keep the same projection and
validation. One File Manager lifetime algorithm selects native or treaty storage;
all tickets, quarantine and retained completion authority still belong to the
shared registered facility.

The SDK guards, trap records, provider request codec, both filesystem drivers,
transfer events and scheduler wake identity preserve the whole native token.
Deferred wakes are a native enum distinguishing driver jobs and transfers, never
a guess based on ID bits. A transfer owns its preflighted continuation lease
through guard refusal; no cleanup allocation, fixed emergency array or slot-index
identity is required. Per-owner storage remains one registered backing per kind.

Provider flight queue preflight follows the actual lower OwnedJob capability
bound, including suspended flights, rather than guessing a total from thread and
file-object limits. This is storage for already bounded driver jobs, not a new
file-object quota. The lower driver job/request/device/module migration remains
open and must replace that dependency when its capability storage grows.

The native operation migration does not enable byte charging, heap budgets,
physical reclamation, paging, multihart or userspace std. The remaining aliases,
process/thread/image capacity, invocation/driver ownership and launch/recovery
gates still precede the page/MMU and allocator stages.

Qualification: 366 scoped host cases pass, including the separately gated
real-image catalog/both-forks read. Canonical SDK cases hold twenty simultaneous
writes, streamed writes and icon groups, observe acknowledged storage reuse via
the authoritative ledger, and reject stale, foreign and cross-kind identities.
Nine native checks pass: real provider writes/copy/exit/rollback/retry, both
endpoint disconnections, two process campaigns, aggregate userspace conformance,
bounded Finder updates, original cursors, Reactor Room and multi-version StuffIt
read/copy/corruption/nested-archive coverage. Native userspace also holds twenty
full-width writers and proves reuse, stale refusal and complete disposal. These
checks cover this migration rather than complete classic compatibility.


### Publication returns retained completion provenance

The shared store returns an opaque publication receipt from the same owner
transaction that installs its payload. It carries the native ID and the exact
owner/kind/registration/slot/generation completion key. Native and treaty
projections use that single result; they do not reconstruct completion authority
with an ordinary live-ID lookup after publication. That lookup correctly rejects
already invalidated objects, even when their payload remains for cleanup.

File operation storage, document batches and newly interned shared targets now
consume the receipt directly. Receipt metadata neither acknowledges lower I/O
nor independently pins a payload or pages. Actual stored payloads and unique
removal guards still own completion; after their release the old receipt cannot
resolve a reused or foreign object. Document-batch retirement preflight counts
retained completions in O(1), including pending objects, rather than enumerating
live batches on every admission.

The motivating handoff is demonstrated through a controlled host invocation
gate after publication metadata unlock. It is a future concurrency contract,
not evidence that native multihart scheduling is enabled: current File Manager
access remains serialized through the interrupt-masked hart0 handoff. No new
privileged-fault recovery claim or allocator/MMU activation follows from it.

Qualification: 371 scoped host cases pass, including five publication/handoff
cases and the actual release-image read. The host gate invalidates after the
real publication unlock: ordinary lookup refuses while the sealed receipt still
resolves retained data, and real completion then revokes it. Root/child scope,
foreign-store, pending-quota and stopped-owner checks retain their typed causes.
Nine native checks pass across provider copy/exit/disconnect/retry, process and
document lifetimes, aggregate Toolbox, bounded Finder, cursor, Reactor Room and
StuffIt. Native/classic ABI, architecture boundaries, formatting and 52-library
API documentation pass; the tested packaged installation remains unchanged.


### Scheduled-thread resource selection

Resource search overrides are one coherent owner/image pair in the scheduled
thread's context, rather than independent machine-global atomics. A scoped
restore touches only its original generation-tagged thread. Assignment clears
the previous selection; actual quiescent reclamation clears it before releasing
the owner. Reusing the context cannot let an abandoned old restore publish the
previous application's resource search chain into a new thread.

The architecture I/O wrapper snapshots explicit resource selection before its
synchronous bootstrap-stack handoff and restores it around the callback. This
transports the subject's resource search context without pretending that the
callback executes on the application's thread. The permanent bootstrap selector
has no reclamation operation, so this cleanup/paint handoff needs neither a
panic nor a fallible fallback. Its private one-shot closure move is confined to
the existing audited architecture stack-switch boundary.

Selections are Copy metadata, allocate nothing per call, and hold no payload or
image lifetime lease on a potentially abandoned application stack. Immutable
images remain in the existing resident cache. Image aliases/pins, resource
origin lifetimes, full four-part module invocation context and dynamic image/
thread capacity remain required migrations; this slice does not reclaim image
backing or enable MMU, multihart, byte accounting or userspace std.

The expanded System bundle crossed the ordinary 4 MiB application resource
payload limit during qualification. Trusted startup resource admission now
uses available resident heap backing as its payload bound, with the existing
map/count bounds and fallible allocations intact. Ordinary application resource
limits are unchanged. Startup refusal reports the file, status and available
backing; exhaustion, missing and translation-required cases retain typed format
identity. The outer boot failure describes System startup rather than claiming
every failure occurred while loading Finder. This is transitional startup
admission on the existing allocator, not enforcement of the approved later
process budgets or recovery reserve.

Qualification: 376 scoped host cases pass, including five selector cases and
the separately gated actual-disk read. Nine public-SDK native checks pass across
provider copy/exit/disconnect/retry, two process campaigns, aggregate Toolbox,
bounded Finder updates, original cursor sprites, Reactor Room and StuffIt. All
17 cold-ROM cases pass, including an enlarged System payload and missing or
malformed media. Native/classic ABI, architecture boundaries, formatting and
52-library API documentation pass. The boot gate recreates catalog timestamps;
executable resource hashes remain identical to the native-tested artifacts and
the final disk is read again. Initial startup-budget, host link and stale fixture
failures are retained in private evidence.


### Module identity domains and authoritative inspection

Loaded-module metadata now lives beside its genuine Module owner ledger in the
shared domain directory. The same boot-wide incarnation issuer used by processes
provides the complete u64 identity; the former 32-slot Modules projection and its
generation array are removed. Driver entry descriptors, dependency calls,
provider ownership, job owner tags, device ownership and backing-file ownership
carry that complete incarnation. Their separate request/job/device/source handles
still use the existing facility tables until their own ledger migration.
Interfaces join the ledger below; a wider owner token alone does not complete
their attribution.

Quiescence closes the module ledger's ordinary admission. Rollback can retire a
domain only when the directory's real live/pending/completion barrier is empty;
refusal keeps its metadata and authority available for completion. Initialization
allocation is fallible and a refused cleanup is reported and retained. Module
code and failed-module diagnostic records still stay pinned until reboot. Actual
image unloading, module-page/account ownership, the four-part invocation context
and M-D12 lower-I/O attribution remain open. Arbitrary privileged-fault containment
is unproven (M-D04), a required future gate.

The native Host descriptor is 48 bytes and carries a u64 module incarnation. Its
layout fingerprint changes while the development ABI remains 1. All modules and
System rebuild together; old layout declarations are rejected before entry.

Public inspection borrows the actual process and module directories through one
reader; no copied owner or ledger catalogue is introduced. An All snapshot holds
both exact directory epochs, represented by the SDK's opaque MembershipRevision.
Changing either included membership invalidates continuation. Own-process reads
carry no module epoch, so unrelated module admission does not invalidate them.
Rows expose a typed domain kind, real contract label and the same ledger's
counters. Requests/devices and other unmigrated objects are never invented as
tracked counts. Published interfaces and their backing join the ledger below. Cross-domain detail keeps the existing System policy
grant; ordinary own-domain and public registration access remain available.

Reactor Room displays App/Module labels using its existing managed row cache and
bounded selection/detail drawing. Module membership changes coalesce an advisory
notice without calling the desktop while the module registry is borrowed. The
normal event pump consumes that notice only after acquiring the desktop; no
sampling timer or per-input directory scan is added.

Qualification: 419 scoped host cases pass, including the actual-disk read,
forty module domains through the public SDK, exact epoch and policy refusals,
live/pending module-retirement barriers and full-width/stale driver entry checks.
Eleven native checks pass: module admission and dependency ordering, provider
copy/exit/disconnect/retry, two process campaigns, aggregate Toolbox, bounded
Finder updates, original cursor sprites, Reactor Room's module selection, power
restart/shutdown/refusal and StuffIt. The module campaign includes explicit
rejection of the obsolete 40-byte Host declaration before entry. All seventeen
ROM checks, native/classic ABI, boundaries, formatting and 52-library docs pass.
Executable resources remain unchanged across final verification; the boot gate
regenerates catalog timestamps and the final disk is read again. These checks
qualify module identity and inspection, not complete driver-ledger adoption or
module fault recovery.


### Driver interface registrations and boot-time cleanup contexts

Published module service interfaces use the shared native value store and the
provider's genuine Module OwnerRef. Resident services use the genuine System
OwnerRef; callback routing's module0 sentinel does not manufacture a resource
owner. DIFC registers growable interface values; DIFS registers one retained
backing per provider domain. The old128-slot DriverInterfaces namespace and
its generation array are removed. Interface publication remains transactional:
fallible growth precedes visibility, and partial commit rolls back its inserted
values before module admission can be retried. Empty failed-publication backing
is released on rollback.

Provider references are opaque native u64 identities throughout dependency
resolution, the driver SDK, broker callbacks, mounts, file-source continuations,
background jobs and power selection. They never narrow into the remaining
u32 volume/request/device treaty tokens. The broker contract's fingerprint
records the widened function signature without incrementing development ABI1;
all packages rebuild together and stale signatures are refused before entry.

The single resident kind catalog accepts a borrowed safe cleanup context.
Desktop-only handlers use a checked adapter over their existing implementation;
boot-time module cleanup can use the actual driver registry without constructing
a desktop. An unavailable required context returns the unique ticket with a
typed refusal, so quarantine and retry remain the ordinary walk. Rust trait
objects remain entirely inside the resident System and never cross the module
ABI. This adds neither a second kind catalog nor a manager cleanup rollcall.

A request retains the interface's validated contract and full resource key at
admission. Poll/cancel therefore remain routable after the interface ID becomes
invalid, with the original caller/provider checks retained. Interface cleanup
refuses while a real request still uses that generation; the unique ticket and
backing stay pending. Terminal completion or acknowledged cancellation ends the
request, and cleanup retry then releases the interface and its backing. Finishing
module quiescence also refuses a nonempty live/pending/completion-fault barrier.
Copied contract metadata is not an unloading proof: module executable mappings
and diagnostic metadata still remain pinned until reboot.

Public inspection reads these real DIFC/DIFS pools from the actual provider
ledger and their registered kind policies. Own-process inspection early-outs
without acquiring the unrelated module runtime. Requests, jobs, devices, DMA,
file-source ownership, shared dependency aliases, image lifetimes, accounted
allocation and full four-part invocation attribution remain staged. Arbitrary
privileged-fault containment is unproven (M-D04), a required future gate.


### Menu cleanup prepares geometry scratch during admission

The native aggregate gate exposed disposal allocating a complete boxed state for
every following menu. Ordinary low-memory refusal left two owned menus live after
Rust Drop. Menu backing now owns one reusable boxed measurement workspace and a
small vector of geometry results. Installation prepares vector capacity before
publishing the next menu; disposal performs no scratch/vector allocation. The
scratch and vector die with their actual registered MNST backing, not with an
untracked manager cache after its ledger acknowledgement.

The shared store splits safe borrows of live owner state and a read-only view of
that owner's existing value index. The view validates full owner/kind/generation
on every lookup, rejects other owners, and disappears when backing is invalid.
It neither clones an object catalog nor issues IDs or cleanup authority. Held
Rust borrows retain payload storage while new lookups reject revoked IDs.

Reflow collects only title/popup/item rectangles. Every definition measurement
must succeed before the disposed ID is invalidated or any other menu moves.
Measurement changes to non-geometry input are a contract refusal. A refusal
preserves every old handle and rectangle; retry clears the staged geometry and
uses the same prepared workspace. The full low-memory native gate remains the
acceptance check; host fault tests separately exercise refusal after one staged
result, unexpected input mutation, atomic preservation and successful retry.

Qualification for this slice: 530 scoped host tests pass, including public SDK
interface/backing inspection, stale and foreign references, a pending request
holding retirement, actual completion/retry, and menu reflow refusal/retry. Ten
native campaigns cover module admission/dependency ordering, host-volume use,
endpoint disconnections, process cleanup, aggregate Toolbox ownership, bounded
Finder selection, original cursor sprites, Reactor Room, external power and
StuffIt. A focused native module rerun also rejects null and u64-misaligned
resolver outputs without writing their canary buffer, then succeeds through the
ordinary safe SDK. Provider output validation follows the pointer's type rather
than a duplicated alignment constant. Lower request/job/device ownership and
module-code reclamation are not claimed by these gates.


### Scoped image reads before backing reclamation

Resident services no longer receive a static AppImage reference or a static
Resource Manager search chain. Higher-ranked callbacks limit image and catalogue
borrows to the service invocation. Loaded-image lookup goes through the same
borrowed catalogue; parsed pictures, icon families, dialog descriptions, menu
state and document declarations leave it as owned values. The catalogue remains
Sync and the scope adds no allocation or extra service query.

Each native resource view captures the selected image and its complete owner
incarnation together. Definition provenance uses that captured source context,
including helper-thread and stopping-process reads, rather than inferring the
source from the thread that happens to execute the callback. Definition bindings
and translation requests retain all 64 bits of the origin. System resources keep
their existing origin convention. Standard File icon preparation keeps its owner
selection around the complete synchronous read and parsing operation.

This is a borrowing prerequisite, not a reclaimable-image implementation. The
fixed eight Once-backed image slots and advertised loaded-image scope projection
still exist. Genuine source aliases, private execution instances, retained
completion authority, code-origin pins, thread-stack quiescence and dynamic image
capacity remain M-D06 migration work. A scoped callback is not itself a lifetime
pin, a resource charge or proof of immutable-frame sharing.


The scoped-read prerequisite passes 471 host cases, including captured-context
selection and full-width translation origins. The optimized native installation
and actual QEMU application switching, aggregate public SDK, cursor/boot visuals,
host-volume and complete panic-report/recovery campaigns pass. Compiled Toolbox
boundaries, formatting and public API documentation also pass. These checks do
not qualify image reclamation or dynamic process capacity.

### Execution pins survive thread retirement

Each current application execution reservation owns an architecture-neutral
execution gate. Atomic admission excludes code writes while any execution pin
exists and refuses immediately during another load. A successful load publishes
its first pin in the same transition that ends exclusive loading; there is no
unpinned interval before thread admission. The private loaded-entry value keeps
the entry and pin together until the scheduler consumes them. Finder's initial
bootstrap load is pinned before its image or thread is published.

The scheduler stores the pin beside the actual stack, with stack destruction
ordered before pin release. EXIT marks the thread retired without dropping that
record. Another thread's interrupt-masked reclamation checkpoint releases the
saved context and stack, then the execution pin. Allocation/admission refusal
drops the unstarted pin; loader refusal drops exclusive admission without
publishing one. A load or pin never waits on a manager lock, allocates per event,
or adds work to input handling. Only initial gate creation allocates metadata.

New applications validate and write their execution bytes once before cache
publication. Malformed code cannot consume a cache slot. Cached relaunches must
acquire the same load exclusion. Existing owner checks and the stopping-process
barrier still protect quarantined callbacks after thread reclamation.

This closes thread execution versus reload exclusion on the current flat
reservations. It does not make source images reclaimable or replace their fixed
cache, register image aliases, pin definition origins, share immutable frames,
account page charges, enable dynamic thread capacity or activate the MMU.
Those remain required model migrations. The gate is a use counter, not another
object-ID issuer or a substitute for ledger completion authority.

Qualification passes 575 host cases (67 VM-dependent cases excluded from that
run), including exclusion, concurrent admission, counter exhaustion, failed-load
retry, atomic handoff and contained host-unwind cleanup. Both real process
campaigns pass, including malformed-dialog recovery, private state, repeated
exit/relaunch and stop/walk/stack-reclamation/domain-retirement ordering. The
optimized installation, application UI, aggregate public SDK and full panic
report/recovery checks pass, as do compiled boundaries, formatting and public
API documentation. Process fixtures now respect the checked display reservation
and compare original menu geometry relative to public desktop bounds. These are
current flat-reservation checks, not image-alias or paging acceptance.

### Provisional process owners precede ordinary image admission

The ordinary launcher finishes its potentially deferred resource-fork read,
then provisions the genuine child domain before loading or binding an image.
A missing image is represented explicitly in the process record and its cleanup
resource binding. Failure at image preparation uses the same owner walk as later
launch failures; its resource scope contains System resources only and restores
the launching thread's previous selection afterward. It never substitutes the
launcher's application image for a child that has not admitted one.

This establishes the subject ledger required by image-alias admission without
creating duplicate domains during deferred I/O replay. It does not yet register
source-image aliases or change Finder's bootstrap admission ordering, fixed
image/thread capacities, accounting or paging. A provisional domain remains
inspectable until its normal ledger retirement barrier completes.

Qualification passes 494 host cases (67 native gates excluded), the optimized
installation, both real process campaigns and the native app UI, aggregate SDK,
cursor, boot, panic-recovery and host-volume campaigns. Canonical SDK cases make
32 repeated missing/malformed-relocation launch attempts in each process campaign,
checking typed errors and their messages, no child publication, preserved caller
identity and restored resource selection. Rust also checks each provisional
domain's actual retirement after its common-walk marker and absence of any child
thread. Boundaries, formatting and public API documentation pass.
