# Memory, application and resource model: first-cut design

Status: §13.1 accepted by Chloe, 2026-10-23; proceeding with §13.2. The complete
RVA23 floor is live. Process identity/directory, owner-walk cleanup and growable
native scheduler records are resident. Original-account allocation headers and
scoped resident metadata routing now extend stack/CPU charges. Selected optimized
native lifecycle/storage/recovery observations and their limits are recorded below. A fixed512 MiB page-backed System heap
is split into480 MiB ordinary and32 MiB protected storage. Independent application
heaps, image-page charges and private process mappings remain pending. Module
allocation forwarding, genuine invocation/resource scopes and module-owned
workers are implemented; bounded native observations are recorded below. The
Host Clipboard development prerequisites are published; complete recovery-service
and cross-CPU policy activation qualification remain separate gates.
Kernel self-virtualization uses owned Sv39 roots and a guarded
System stack. Graphical boot now pins an independent terminal panic/OOM kit;
complete Save/Alert/Finder recovery kits and module/privilege gates remain open.
This document describes the implemented foundations,
architectural decisions and remaining migration gates.

## What landed

The `no_std`, `forbid(unsafe_code)` **mac-kernel-model** crate is internal
kernel infrastructure, not an application SDK. Shared representations and
algorithms belong here; architecture-specific mapping, allocation, interrupt and
scheduler behavior must be exercised through the actual target implementation.
Host-linked service/model results are not native OS acceptance. System consumes
its execution-use gate, physical pool and page-owned kernel roots; other
families remain staged until their resident consumers land. Linking the crate
alone does not activate its page or allocator model.

| §13.1 family | Code | Current scope |
|---|---|---|
| Buddy pool | `crates/mac-kernel-model/src/buddy.rs` | Caller-supplied metadata and order heads, split, O(1) unlink, coalescing, unique RAII block leases; never touches free-page bytes |
| Page layer | `…/pages.rs` | Platform geometry, typed addresses, permissions/ownership, borrowed boot reservations, largest-leaf mapping plan, reserved `PageBox`/address-space root |
| Domains/accounts | `…/policy.rs`, `mac-object-table/src/domain.rs` | Boot-monotonic incarnation, canonical process/module accounts, checked admission and original-account receipts; execution plus attributed resident heap/metadata; bounded native observations below |
| Facility v2 | `mac-object-table/src/domain.rs` | Owner-index-authorized per-kind bitmap pools, generations, rotation, retirement, capacity policy, release tickets and registration shapes; managed controls use owner-backed value storage |
| Shared aliases | `mac-object-table/src/aliases.rs`, `system/src/images.rs` | System-owned immutable image sources and registered process aliases; source reads, definition bindings and execution pins retain backing through actual cleanup |
| Process/thread | `…/process.rs` | Address-space, image/memory/legacy capabilities, dynamic threads, saved manager slots, backend frame, vector marker and discovered save-area layout |
| Execution use | `…/image.rs` | Nonblocking load exclusion, fallible gate allocation, atomic load-to-pin handoff; real scheduler records retain pins until stack reclamation |
| Control zone | `…/zone.rs` | Same-owner instance/zone binding, configured zone header, host-forwarding allocator contract; no allocator installed |
| Uniform layout | `…/policy.rs` | One checked range record emits named linker symbols; no unapproved addresses or duplicate linker constants |
| TE2 rope | `…/text.rs` | Immutable balanced chunk tree, byte/hard-break augmentation, shared snapshot roots, logarithmic chunk lookup; no edit/rebalance/control/file-provider migration yet |
| Emergency kit | `…/emergency.rs`, `system/src/machine/emergency.rs` | Checked requirement manifest plus boot-pinned terminal display/stack/input backing; complete protected recovery reserve and Save/Alert/Finder kits remain pending |

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
`InvocationGate`. The native adapter masks/restores local SIE with compiler
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
then-static backing. The fixed page-backed heap described at the end now uses
the same boundary with the original-account receipts described below.
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

The following sections record successive §13.2 migrations and their remaining
gates; they do not claim the complete model. Registered image aliases replace
the fixed source cache. Scheduler metadata now grows until allocation refusal;
the fixed heap budget and flat image arena still bound physical admission. The real process fixture
checks native and Pascal stale IDs across relaunch, fresh incarnations, shared
app-menu actions and full-width window resource ownership. The original directory test was host metadata coverage. The later native
capacity campaign now runs ten applications together, without claiming MMU
isolation or memory-scale acceptance.

`ReleasedResource` records invalidation-before-dispatch. Registered controls now
use the live dispatcher described below; this is not a proof of quiescence for
the remaining managers. Full alias and account integration remains work beyond
the active image pins and stack/CPU receipts. A drained observation alone cannot
authorize page reuse.

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
coalesces; its former static backing and still-uncharged ownership are the problem. Xtra's
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
remain tunable. The release RAM and fixed page-backed System heap are active;
the process/module limits now also cover attributed resident heap blocks. The
protected32 MiB partition is active, while complete private-heap/image enforcement
and recovery-service kits remain behind their accounting/recovery gates. Physical admission can refuse a
chosen heap capacity on a smaller RAM profile without silently changing policy.

Other values, including the proposed minimum/maximum ranges below, remain
**review proposals**. Evaluate them with real image/control/account usage,
including Res Edit and a 200 MB Text Edit document, before enabling. Approval of
the five starting defaults does not select virtual addresses, stack/control-space
sizes, Finder's recovery share, or emergency-kit concurrency. Stack policies were
subsequently selected on 2026-10-26: both System-per-hart and application-per-thread
defaults are 4 MiB; System bounds are 256 KiB–8 MiB and application bounds are
64 KiB–8 MiB. The checked platform record emits both linker symbols and Rust
constants. Application/idle stack allocation consumes that default; the System
stack and mapped guards are active in native bootstrap and service entry;
private process mappings remain a separate paging gate.

| Configured policy | Minimum / initial default / maximum proposal | Reason |
|---|---|---|
| Physical RAM for native release VM | 512 MiB / 8 GiB / platform-discovered | Target class is ~8 GiB; small guest remains a deliberate test profile |
| Fixed System heap | 128 MiB / 512 MiB / 1 GiB | Must hold resident state plus charged process kernel allocations, including high-water reserve |
| Protected recovery floor within System heap | 16 MiB / 32 MiB / 128 MiB | Measured kit + Finder restart, unavailable to ordinary process charges |
| Finder restart portion of that floor | 8 MiB / 16 MiB / 64 MiB | Subset of floor, never added twice; final value measured from restart peak |
| Process kernel account | 4 MiB / 16 MiB / 128 MiB | Controls, tables, zones, TCBs and frames; admission also checks global remainder |
| Process app committed heap budget | 4 MiB / 64 MiB / 2 GiB | Grow on demand; a 200 MB file uses file-backed views, not a forced whole-file copy |
| Application heap growth minimum | 16 KiB / 64 KiB / 2 MiB | Rounded up to target geometry; bounded by selected process budget |
| Per-thread fixed stack (approved 2026-10-26) | 64 KiB / **4 MiB** / 8 MiB | Static per thread, charged app account, freed after switch-away |
| Per-hart System stack (approved 2026-10-26) | 256 KiB / **4 MiB** / 8 MiB | Guarded resident stack is active; per-domain charges remain a heap-stage gate |
| Designated per-control space | 1 MiB / 16 MiB / 128 MiB | A sub-budget of process kernel account, never independently guaranteed physical memory |
| TE2 owned threshold | Tunable 0–100%, default 40%, bounded by a tunable **~3.2 MiB default ceiling** | Ratified 2026-10-23 calibration: owns strictly below both limits; at either limit uses view. The ceiling is 3,355,443 bytes (3.2 MiB rounded down), even if control space grows |
| Process/thread/handle counts | Grow until account or memory refusal | No fixed-eight or guessed high replacement constant |

Separate module-worker defaults were selected on **2026-10-27**: a configurable
**4 MiB worker stack** and **16 MiB kernel budget per module**. They are distinct
module policy values, independent of process budgets and the module metadata's
existing callback-stack declaration (at most 64 KiB). This selects defaults, not
new minimum/maximum ranges. Worker activation still requires actual rounded
stack/CPU/metadata backing charges, original-account receipts, native context
admission and recovery support; policy selection alone supplies none of those.

Selected values must satisfy cross-profile constraints: reserve <= fixed heap,
Finder portion <= reserve, control space <= kernel budget, growth <= app budget,
stack budget available before spawn. The current constructors do not select a
profile, and no complete live profile validator is claimed yet.

Uniform layout proposal: one app interval/preferred image VA for all processes;
private supervisor process-kernel interval; shared supervisor module interval;
shared supervisor kernel interval containing the physmap. Store the chosen
ranges once, validate through the architecture, generate both Rust input and
linker symbols in xtask, then rebuild all producers. Concrete Sv39 VA reservations are approved below; additional healthy policy
ranges still require explicit selection. Today's physical 0x81… application
addresses do not become future virtual addresses. The emitter now supplies the production System link address and its guarded
stack window. Per-process private images and heaps remain migration work; the
trusted flat application arenas are explicitly temporary.

Image resource residency proposal: **resident-with-image** for v1.
Last image release drops code/resource backing
immediately; no unaccounted warm cache. Shared immutable image backing plus
per-process writable sections must replace today's never-unloaded `Once` slots.
Demand paging is a separate [DECIDE], not an implicit change here.

## Emergency services: draft enumeration for ratification

The complete heap-stage kit must be reserved and pinned before fonts, modules,
Finder or ordinary service allocation. Its first installed consumer, the terminal
panic/OOM kit described below, is admitted even earlier: after permanent page
roots and physical pools, before GlobalAlloc initialization. A terminal-kit
admission failure refuses graphical boot and reports its typed cause through the
bootstrap UART. It does not enter ordinary services with an incomplete kit.

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
The physical buddy pool now selects NativeInvocationGate explicitly on every
metadata access and allocation-lease disposal. Its first resident allocations
are prepared app/idle stacks; single-hart interrupt/preemption masking precedes
every pool lock. This does not establish multi-hart discipline or activate paging.
Host-thread stress alone cannot establish the native interrupt contract.

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

The TE2/Text Edit migration is **on hold** pending the replacement common-controls
architecture. Its earlier implementation shape is not an active work order.
Paging, heap/reserve and domain work continue independently; existing control
safety and lifetime contracts remain in force.

Reactor Room is now the first customer for stage-2 inspection APIs and a visible
acceptance surface for each later stage. Implement its application and native
data controls alongside the services, following [Reactor Room integration](REACTOR.md).
Enumeration, capabilities, revisions and cheap
cumulative counters are part of the model, not a private inspection backdoor.

Keep §13.2 stages explicit: ISA floor; facility/manager migration; owner walk and
unified rollback; dynamic process records; live page layer; heaps/reserve/control
zones; module domains/resource scopes/individual driver extraction/**Host Clipboard Extension**;
TE2+Text Edit; P13 privilege campaign; **Multi-Scheduler Extension scheduler takeover in its own isolated
commit after the full model boots cleanly**. No capstone scheduler feature slips
into a memory change.

The [Host Clipboard Extension](HOST-CLIPBOARD.md) is a required proving customer
for System extensions, not optional follow-up work. It must be independently
packaged in `System Folder/Extensions`, register through the generic domain/ledger
and service machinery, preserve local Scrap when unavailable/disabled, and qualify
real bidirectional Unicode sharing on Linux and native Windows. Registration
retirement, pending-I/O cancellation and stale-generation rejection are part of
that acceptance. The extension supplies its icon family through bundle resources;
System displays it in the classic startup strip, with the standard extension icon
as fallback. The initial code-pinning policy remains as documented; safe hot code
reclamation is not implied by disabling the bridge.

The Multi-Scheduler Extension is the required scheduler/SMP proving customer,
kept as its own isolated capstone after the other active model gates pass.
The resident scheduler remains the dependable single-hart boot implementation.
The loadable Multi-Scheduler Extension supplies replacement scheduling policy and
decides when to activate additional harts; the fixed core exposes architecture
mechanisms and enforces safe handover. Per-hart context, MMU and interrupt/lock
proofs gate activation. Later async I/O APIs likewise belong to a loadable
extension over the core's owned completion/cancellation/wakeup mechanisms.

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
virtual layout activation, physmap mappings, later image allocations and
page-backed heaps remain their staged work. The ROM and System still execute in
M-mode with identity addressing; kernel self-virtualization needs the architecture
privilege transition in addition to SATP/table setup. No user-isolation claim
follows from the reservation list.

Physical-map qualification includes 21 parser/ABI/build host tests, 219 public
SDK host cases, optimized whole-world packaging, seventeen ROM boot/rejection/
fallback cases, actual 128 MiB/512 MiB/8 GiB boots and a pre-heap 64 MiB refusal.
Native/classic ECALL smoke, boot pixels, aggregate userspace and Reactor Room on
Linux/Windows observations were recorded at that stage. These historical host
results are not current OS qualification. The raw-memory ABI bootstrap and its
standalone command were subsequently retired when the real ROM machine-monitor
handoff became mandatory. Current native evidence must boot that production ROM;
a fabricated monitor or alternate bootstrap does not establish the contract.

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

### Finder bootstrap uses its existing process owner

Finder now provisions its process domain before image admission too. Bootstrap
binds the image and passes the same process directory and genuine owner into
Desktop construction. The old constructor that created an already-imaged process
is removed. A directory-minted resource selection encloses desktop initialization;
the helper returns, drops its guards and restores the System-only bootstrap
selection before the scheduler permanently leaves that stack. Bootstrap checks
that the selection was released, rather than keeping an owned admission guard
on a stack that will never unwind.

Current image-selection metadata now lives in the actual thread context, assigned
before publication and cleared only after quiescence. It is removed from the
scheduler record. Synchronous I/O captures complete default or explicit source
selection before changing executor. It does not reacquire the scheduler lock:
file-transfer completion-before-park already holds that lock while checking I/O
readiness. System/idle contexts without an explicit source see only System
resources, rather than implicitly selecting Finder through slot zero.

This is coherent source selection and owner-first admission, not registered
image ownership. Image aliases/eviction, dynamic capacity, four-part module
invocation, page/account enforcement and MMU activation remain staged gates.
Boot initialization failure remains fatal; this does not claim Finder restart
or arbitrary privileged-fault recovery.

Qualification passes 494 host cases, the optimized world build, both real
process campaigns (including concurrent-copy rename/move and bootstrap scope
ordering), application UI, aggregate SDK, cursor/boot visuals, panic recovery
and host-volume workflow. Boundaries, formatting and public API docs pass.
The private lifecycle campaign caught the scheduler-lock recursion in an initial
capture implementation; current-context reads now avoid that lock entirely.

### Registered immutable sources and retained image aliases

The fixed eight-entry, never-evicted source cache is replaced by the shared
alias facility. Immutable source resources are System-owned targets (IMOB/IMOS);
each process admits its own generation-checked image alias and backing
(IMAG/IMST) under its existing genuine owner. Identical sources share immutable
resource storage, while execution reservations remain private. The initial
policy is resident-with-image: releasing the final alias drops the source;
there is no warm source cache.

Admission returns the actual registered completion receipt. Thread context and
process records carry that receipt rather than recreating authority from an ID.
Resource operations hold a scoped read through their complete invocation;
cached application definitions retain their source until the process has no
remaining non-image resources or executing threads. The final image cleanup
uses a System-only selection so it cannot retain its own last source pin.
Stopping invalidates public aliases immediately; retained completion reads
remain available only through the original receipt. Physical placement includes
pending and deferred reservations, preventing reuse before quiescence.

The scheduler destroys the saved stack before releasing execution and source
pins. Retirement acknowledges the actual deferred alias only after its readers,
threads and other registered resources are gone. Failed loads remain owned by
the provisional process and follow that same walk. Cleanup allocates no new
tickets and does not scan active images on input events.

Native resource scopes carry a separate selection tag and the complete 64-bit
image ID. Returned resource views record their actual source; keeping a view
does not keep an exited application alive or redirect a later read into another
application. Public process snapshots expose the read-only image identity.
Native descriptors validate a generated ABI-layout fingerprint while the
unreleased ABI stays at version 1; stale native packages require a world rebuild.
Classic resource-fork bytes and classic calling conventions are unaffected.

This closes registered source ownership and flat execution reservation reuse.
Shared read-only executable frames, private writable page mappings, byte charges,
dynamic concurrent process/thread capacity and MMU activation remain separate
gates. Source sharing alone is not executable-frame sharing or process isolation.

Qualification passes 578 scoped host cases (67 VM-dependent cases excluded),
including one-slot alias reuse and retained completion authority. Both native
lifecycle campaigns launch 16 distinct files, reject full-width stale and
foreign-kind image IDs, reject saved resource views after exit, and verify
stack reclamation precedes domain retirement. Each probe draws through its own
application CDEF; all 16 source identities remain distinct while the reclaimed
definition reservation is reused. The preemption fixture now waits for the
child's explicit public start handshake and fresh switches, excluding startup
switches from its no-trap progress evidence.

The optimized installation, application UI, aggregate SDK, cursor/boot visuals,
panic recovery and writable host-volume workflow pass. Native/classic ABI smoke,
all 17 ROM boot/rejection cases, compiled boundaries, formatting and public
documentation (52 crates) pass. The code-resource codec consumes only the
allocation-free ABI contract to validate its layout fingerprint; the compiled
boundary gate still forbids service or allocator implementations in clients.

The QemuRISC-V flat heap is now 10 MiB, defined in its platform record and still
bounded by the existing System linker reservation. At 800x600 the native
QuickDraw-plus-Res-Edit campaign exhausted the previous 8 MiB budget while
opening a resource document; its picker left less than 100 KiB of headroom.
The display backbuffer lives beside the frontbuffer and tracking scratch in a
three-frame platform reservation; it no longer occupies System image BSS. One
checked record generates their linker symbols, Rust constants and boot reservation.
The image ceiling and Res Edit linker ceiling end before this reservation. A
9 MiB intermediate profile still refused the full System font-view workload.
This is an interim measured capacity adjustment, not activation of the approved
512 MiB page-backed System heap, recovery reserve or domain charges. Res Edit's
large document array now lives in synchronized per-instance storage with an
owned cleanup guard, removing its separate UI-stack overflow.

Native packages now use explicitly marked RRF1 resource forks with full u32
payload offsets. Original Macintosh forks keep their classic 24-bit decoder and
encoder; storage and copying do not reinterpret or rewrite their bytes. Both
formats share checked index/stream parsing and independent allocation budgets.
Res Edit's public file-format query and JSON archive field preserve the chosen
encoding through rebuild. Host SDK acceptance reads a resource past 16 MiB;
the native Res Edit workflow verifies exact exported/rebuilt fork bytes.

Qualification: 490 scoped host cases pass, with 67 native tests excluded from
that count. Separate guest campaigns pass for process lifecycle/rollback,
aggregate Toolbox, Res Edit, application/cursor/boot/panic UI, writable host
volumes, classic StuffIt and external module admission. The ABI harness and all
17 ROM cases pass, including a valid System fork over 5 MiB. Resource/font
provenance checks, compiled boundaries, formatting and 52-crate public docs pass.
This container milestone does not establish original 68k execution, arbitrary
privileged-fault containment, dynamic process capacity, page-backed heaps or MMU
activation. Lower driver-request/device/source lifetimes remain the next model
migration.

## Module dependency claims join registered cleanup

`DDEP`/`DDST` register dependency claims and their backing in the dependent's
actual module ledger, beginning before entry. Each claim binds the selected
provider/interface incarnation and its genuine registration key. Per-module
issued-ID indexes keep resolution and authorization bounded by that module's
declarations; they contain no duplicate payload or separate ID authority.
Optional absent dependencies remain absent, and wildcard claims do not retarget
when a provider withdraws. The retained claim, including one already invalidated
by a cleanup walk, blocks provider/interface retirement until actual release.
Refusals identify the dependent and instruct the caller to unload it first.

Module rollback and finalization use the registered owner walk. A checkpoint is
allocated before publication and retains a unique ticket if quarantine itself
refuses; typed checkpoint state distinguishes busy, retained and interrupted
cleanup. Unknown release providers keep their real tickets pending. The host SDK
transport drives its actual catalog in its full Session context, outside a
narrow Registry borrow; it does not construct another catalog or ID issuer.

Scoped host/failure checks and canonical public inspection acceptance pass,
including registered claim enumeration, refused admission, exact provider
selection, invalidated-claim retention and repeated rollback. Native external
module, writable host-volume, StuffIt, process lifecycle, aggregate SDK, cursor
and panic campaigns pass, as do compiled boundaries, formatting and public docs.
Physical module code still remains resident until reboot. This slice establishes
logical provider/interface retention; broker requests, owned jobs, devices/DMA,
backing sources, physical code reclamation and the remaining invocation/page/heap
gates retain their separate migration work.

## Driver dispatches return through owned guards

A dispatch now borrows its originating queue for the entire invocation. Rust
rejects moving or destroying that queue while its dispatch exists. Drop returns
uninvoked work, settles a returned callback and its wake notice, or quarantines
an interrupted invocation with its actual owned buffers and broker ticket.
Cancellation before an uninvoked guard drops never starts the unsent request.
Foreign-queue refusal cannot redirect the guard's return, and double invocation
is rejected before another callback runs. Return/quarantine needs no allocation:
admission reserves notices and allocates the node's own retention link.

The queue owns one architecture invocation-gate/lock boundary, including Drop;
no queue guard crosses foreign code. Readiness changes update atomic flags, and
empty/not-notified/not-failed paths exit without taking that lock. Normal
completion keeps its immediate waiter wakeup. Native dispatch cleanup no longer
expects a successful return. The host unwind test retains interrupted work and
reports M-D04's unproven privileged-fault recovery gate; it does not qualify
native recovery from an arbitrary privileged fault.

Scoped host tests, the origin-lifetime compile-fail check, host-thread cancellation
during a live callback and native provider/archive/module/process/aggregate/
cursor/panic campaigns pass. Compiled boundaries, formatting and public docs pass.
That guard milestone retained the generation table and numeric delivery policy.
The following migration establishes registered job subjects and receipts; broker
requests and other lower I/O remain staged. Neither milestone activates MMU or SMP.

## Owned driver I/O subjects and job receipts

Queued driver jobs use registered `DJOB`/`DJST` stores in the real subject's
resource scope. Native u64 IDs and publication receipts travel through the queue,
owned jobs and typed scheduler wake/defer identity; there is no projected native
job transport or separate payload table/ID issuer. Publication captures a
completion receipt before ordinary invalidation; detached callbacks and resident
continuations retain that receipt. A stale public ID fails immediately while
only the original receipt can reach a retained completion. Native payloads and
unique cleanup tickets remain held until the lower ticket actually terminates.
Pending jobs stay in their authoritative backing; extracting removal guards
before ACK could detach that backing and reject a legitimate follow-up Abort.
Only terminal completion takes and destroys the payload guard. Refused
acknowledgements retain their exact ticket and typed cause, with a bounded
change-driven report rather than repeated polling or fabricated success.
A cancellation request, timeout or missing waiter never constitutes that ACK.
The native queue grows until memory within its explicit owned-buffer budget.

`DIOP`/`DIOB` provide an admitted root for a resident operation on behalf of a
client. An existing transfer inherits its real scope instead. Persistent provider
forks, writers, streams and icon groups retain their I/O subject before publication,
so post-exit Close/Abort does not reopen ordinary app admission or charge work to
a guessed current thread/System owner. Follow-up provider jobs clone the original
subject. A synchronous broker frame now carries that admitted subject through
nested module calls and the bootstrap I/O stack handoff. Provider-backed source
reads inherit the incoming operation, while the source retains its admitted
module subject for independent close/cleanup. Delivery policy and waiter identity
remain separate from this allocation/lifetime authority. Full execution, fault,
resource-search and byte-accounting context remains a later requirement.

The root store owns only a weak reference to the operation's retained-use state.
An owner walk transfers the actual removal guard to that state while owned uses
remain; the final use releases the guard after its payload use ends. There is no
strong-reference cycle, reference-count guess, cleanup allocation or invented
completion. A dispatcher arriving after the final use still owns and settles its
unique outstanding ticket. All accesses use the architecture invocation gate;
foreign callbacks hold neither the queue nor registry lock. Ready entries carry
their original receipts directly, and empty queues retain their atomic early-out.

The shared facility reports actual stopped-owner completion after payload
destruction and metadata unlock. Its native adapter posts a coalesced retirement
wake; live-owner releases do not wake the reaper. Completion retry and delayed
ticket routing use the same notification. This closes the gap where a final I/O
root/backing could finish after the old manager-specific wake and leave an exited
process waiting indefinitely. Notification cannot acknowledge I/O or run teardown.

Devices/DMA and backing-source records still require their own registered
ownership/ACK migration. Full module invocation/accounting, physical
module reclamation, dynamic process capacity, pages/MMU, page-backed heaps,
privilege separation and multihart remain subsequent gates. These job and scope
mechanisms do not establish arbitrary privileged-fault containment (M-D04).

Qualification: 574 scoped host cases pass (67 native-only gates excluded),
including 33 focused job/scope cases, foreign-ticket refusal/retry and the exact
stopped-owner follow-up admission regression. Native provider acceptance passes
twice, including real owned scope counts, transfer exit rollback, forgotten
forks/writers/watches, successful final retirement and unmount/retry. Both
endpoint-disconnect cases, classic archives, module admission, both process
campaigns, aggregate SDK, original cursor sprites and panic recovery pass.
Compiled boundaries, formatting and 52-library public API generation pass.
These checks qualify this job/subject slice; they do not close the remaining
lower-I/O, memory/accounting, privilege or multihart gates listed above.


## Synchronous I/O subject frames

The native broker installs its queue's admitted subject in the executing OS
thread's context for exactly the synchronous callback. Nested broker/source
calls read that capability rather than resolving a numeric caller or charging a
stacked read to the provider module. The bootstrap I/O stack handoff transports
the same subject independently of its resource-selection metadata; the executor
identity remains the actual System I/O thread.

The shared selection slot now supports owned capabilities as well as copyable
metadata. RAII restores the previous selection on every normal/error return;
the slot's genuine thread identity prevents an old restore from modifying a
reused context. Replacement/reset destruction runs after unlocking the slot.
Cloning an admitted subject does not allocate another root or reopen ordinary
client admission. Pending operations retain their own subject in owned job state;
no stack-frame guard or borrowed buffer survives callback return.

This is subject transport, not completed broker-request registration, all four
invocation identities, byte accounting or arbitrary privileged-fault containment.
M-D04 still applies: host unwind tests demonstrate the Rust guard, not recovery
from arbitrary native privileged faults. Those faults remain a required future
recovery gate. Thread-local transport does not establish multi-hart activation.

Qualification for the frame transport: the scoped host suite passes 580 cases
(67 native-only gates excluded), plus an additional destruction-under-lock
regression and the final Start/Poll/Cancel subject assertion. All 11 context
selection cases pass. Native provider workflow, classic archives, module
admission, both process campaigns, aggregate SDK, cursor sprites and panic
recovery pass; compiled boundaries and the optimized image pass. One overlapping
provider run reached the unchanged host outcome deadline before lifecycle
completion; its sequential retry passes. Concurrent acceptance-harness timing is
not qualified by that retry. No deadline was increased, and the failed attempt
remains part of the evidence.


## Registered broker requests

Broker requests publish `DREQ` payloads and `DRQS` per-domain backing through
the native shared facility. They inherit the incoming admitted I/O scope;
resident caller policy, module declarations and the executor's identity remain
separate. A top-level module callback outside a queued operation admits its real
module ledger once, and nested callbacks inherit that frame. The request store
is growable until memory; it no longer has the projected 256-request ceiling.

The System retains each publication receipt with the request. A continuation
requires that receipt, the original admitted scope and the unchanged caller,
provider, class and operation. The wire ticket only correlates the invocation.
Ordinary IDs fail immediately on invalidation; another operation in the same
process cannot drain a request by presenting its ticket. Wrong-subject refusal
retains typed identity and reports that the original operation capability is
required. Existing module-policy and owner checks remain in force.

A preallocated slot retains the walk's unique cleanup ticket while lower work
is pending. Payload and backing stay in their authoritative store until actual
terminal acknowledgement, including acknowledged cancellation. If completion
precedes dispatch, released metadata remains until the unique ticket arrives.
Refused retirement retains its typed cause and ticket for the storage walk's
retry; unchanged failures do not flood the log. Request records retain exact
interface pins through actual cleanup. A missing provider acknowledgement leaves
quarantine pending rather than reporting that cancellation completed.

Serialized-provider occupancy is cached by the facility-issued interface ID.
Only admission and terminal acknowledgement change it; admission does not scan
all requests. Terminal transport acknowledgement releases serialization even
when the ledger's cleanup ticket has not yet reached its dispatcher. Interface
and module teardown inspect retained requests at their change boundaries.

The common catalog dispatches request cleanup with either the module registry
context or the native runtime adapter. The latter uses `TryState::with_owned`,
returning the unique ticket and typed access refusal if it cannot enter. Neither
queue nor registry borrows cross a foreign callback; cleanup does not allocate.

Native broker Call is 64 bytes with a u64 request ticket. The SDK, device/source
correlation, driver implementations and host companion all carry it without
narrowing. The host packet header is 52 bytes. Current development ABI stays 1;
contract fingerprints and explicit header-size validation require a complete
matching rebuild. There is no older decoder. Classic external file formats and
legacy object reference projections are unaffected.

Public SDK conformance enumerates both request registrations without inspector
privilege. Native provider tests inspect real request births in the requesting
app ledger and require requests/scoped children to be drained after ordinary
fork operations and process exit. Internal tests cover wrong subjects, stale and
foreign IDs/tickets, invalidation before and after completion, retained backing,
interface pins, serialized admission, common owner-walk retry and growth past
300 simultaneous requests. These mechanisms do not close device/DMA/source
ownership, full invocation/accounting, physical
module reclamation, pages/MMU, heaps, isolation or multihart gates. M-D04's
arbitrary privileged-fault containment remains unproven.


Process retirement treats partial asynchronous acknowledgement as progress.
The native reaper still runs image-origin completion, genuine quarantine retry
and saved-manager parking. It then checks the ledger's death barrier before
asking the directory to remove a process. Pending resources no longer cause a
failed removal and a full per-pool diagnostic on each child completion. Actual
cleanup failures remain typed and reported, and the shared post-ACK wake still
drives final retirement. Canonical native provider acceptance rejects premature
directory-retirement attempts during otherwise valid exit cleanup.


Qualification: 680 host cases pass across the shared facility/model, System,
public Toolbox, driver SDKs/providers and host protocol (67 native-only gates
excluded). All 23 focused registry cases pass, including 300 simultaneous native
request identities and the actual common owner walk. Ten native acceptance cases
pass: provider read/write/lifecycle and cold restore, classic archives, both
endpoint disconnects, module admission, both process campaigns, aggregate SDK,
cursor, panic recovery and external power restart/shutdown/refusal. Optimized
image, compiled ownership boundaries, formatting and 52-library API docs pass.

The provider campaign's transport watchdog now allows 30 seconds without a new
unique fixture milestone, with a 120-second total cap. Repeated kernel logs cannot
extend it. This replaces the old blanket 30-second limit on the entire expanding
correctness campaign; it is not a UI latency claim. The guest's five-second exit
cleanup assertion and all functional/ownership acceptance remain unchanged.
Phase timing is retained as evidence. The power capture also uses the checked
display width for application-menu input, matching the 800-pixel desktop.


## Native job capacity and return reservations

`DJOB` now uses the native shared store directly. All job identifiers, owned
completion receipts and scheduler `DeferredIo::DriverJob` keys retain their full
u64 identity; file-transfer and job wakeups remain distinct typed variants.
Classic object projections retain their own original contracts. There is no
native 32-bit job projection or 128-job slot ceiling.

Admission reserves ready/completion-notice capacity for every retained job before
publication. A callback detached earlier may return after more jobs are admitted;
its RAII guard can restore or notify without allocating. Metadata/retirement
reservation failure still returns the original unsent owned work. The existing
1 MiB aggregate owned-buffer budget and per-request payload bounds remain explicit;
page-backed per-domain byte accounting is a later gate.

Thread-owner discard walks the original completion receipts in native-ID order.
The cursor remains valid as entries are removed, uses no fixed scratch array or
cleanup allocation, detaches dead waiters from resident-owned mutations and waits
for actual provider cancellation acknowledgement. Ordinary IDs still fail after
invalidation; only retained receipts drain late results. Tests force native slot
reuse using the actual slot/generation keys rather than interpreting ID bits.

Focused tests admit 320 jobs with older detached pending callbacks, then discard
all through actual cancellation ACK. Another case returns 300 completed guards
and verifies every exact notice/result while the deque capacities remain unchanged.
Existing foreign-return, refusal, unwind, stopped-owner, fairness, no-change and
scope cases continue to exercise the same queue and shared facility.


Qualification for native jobs: 593 scoped host cases pass (67 native gates
excluded), including all 36 focused job/scope cases, plus 10 native cases covering
provider read/write/exit/cold restore, classic archives, both endpoint disconnects,
module admission, lifecycle, aggregate SDK, cursor, panic and power recovery.
Optimized image, compiled boundaries, formatting and 52-library docs pass.
This removes the native job projection/capacity gate; device/DMA/backing-source
ownership, complete owner-walk recovery, dynamic concurrent processes, pages/MMU,
heaps, full invocation/accounting and isolation remain staged.


### Registered device aliases and DMA retirement

Physical device targets now live in the System ledger (`DVOB`/`DVOS`). Admitted
modules receive native full-width aliases (`DVAL`/`DVAB`) through the shared
alias facility, with independent original bus-match and owner checks. Native
DMA records (`DMAP`/`DMST`) retain the exact claim's authenticated publication
receipt. There is no per-manager ID issuer, 64-claim ceiling or 128-map ceiling.
Probe ordinals remain inventory labels, not object authority.

All six kinds use the ordinary public registry and common cleanup walk. A
mapping counter prevents releasing a claim before its actual dependent mappings
settle. Cleanup issues one successfully submitted reset per claim, probes once
per retry, and retains typed refusals and the unique ticket in the existing
quarantine when reset or acknowledgement fails. An invalidated claim remains
exclusive; late work cannot attach to a newly issued claim. The real hardware
stop ACK permits retiring a dead claim even if its reset waiter was lost. That
does not acknowledge an outstanding broker request: its separate DREQ ledger
barrier still applies. Abandoning the manager invalidates and quarantines live
maps/aliases before store destruction; the System targets remain pinned.

The VDEV SDK, resident service and both hardware drivers use u64 native IDs;
the 52-byte description and unchanged-size 24-byte mapping are documented in
FORMAT.md. The module layout fingerprint rejects pre-change packages before
entry; the unreleased ABI remains 1 and all packages rebuild together.

This closes registered device/map identity and positive-stop retirement policy.
It does not claim physical DMA-page pins, reclaimable module images, autonomous
hot-unload, IRQ-driven reset progress, arbitrary privileged-fault recovery or
multi-hart safety. The platform bus owns hardware access; invocation/context
and the later page/heap stages retain their explicit gates. Backing-source
records and full teardown/Finder recovery remain in the current lifetime stage.

Qualification covers forced single-slot claim/map reuse, stale and foreign IDs,
320 simultaneous mappings, delayed/absent hardware ACK, failed reset writes and
stop probes, arbitrary cleanup order, retained unique tickets, late reset work
and manager abandonment. Canonical public-SDK conformance reads all six ordinary
registrations. Native campaigns exercise the actual transport/power device SDK,
old-device-layout rejection before module entry, both transport disconnects,
provider operations, classic archives, process cleanup, cursor latency and panic
recovery. These are the current hardware/SDK cases, not exhaustive bus support.


### Registered backing-fork capabilities and scoped provider claims

Each native filesystem module owns its independent leased-fork capabilities in
`FSRC` with `FSSB` backing. Shared native issuance, authenticated publication
receipts, preallocated cleanup intents and owned read/close plans replace the
private source table. Both native and module callers use u64 source tokens;
FORK keeps a 24-byte request with its source in bytes 0..8, and module layout bit
59 rejects stale packages before entry. Development ABI stays 1.

Backing file marks join the selected module's admitted DIOP scope before Open;
a known-unopened Catalog placeholder is retired and reissued in that scope.
Provider reads retain the actual incoming operation's subject, while independent
close retains the module source subject. The existing resident File Manager
flight remains the owner of app-initiated asynchronous close after app exit;
common owner cleanup has a separate bounded resident actor. Neither path drops
a pending backing or infers completion from time.

The lower provider has a registered scoped DDEP claim in the source module's
ledger. It grants lifetime only, leaving declared service resolution policy
unchanged. Scope-active cleanup retains the unique dependency ticket. The
unique root's positive closure, observed after metadata unlock, coalesces normal
claim retirement without scanning dependencies on every input or app I/O root.
File-object cleanup now uses the same positive scope-closed evidence, rather
than treating failed admission validation as proof of closure.

Read/close plans fail closed on loss and return intact on a foreign completion
refusal. Real read ACK restores source readiness; a real close ACK retires it.
Close admission holds the validated file mark across fallible queue publication,
so refusal leaves it unchanged. Definite terminal close failure permits close
retry while reads remain refused. Actual replies survive temporary local
retirement refusal; an already-consumed job is never polled again. Uncertain
read flights remain resident for late lower acknowledgement.

Close intents and pending close continuations share a preallocated FIFO; orphan
read returns preserve their order too. A stalled provider cannot repeatedly take
the place of another source's cleanup. Queue publication and the empty-work
wakeup transition use the same metadata gate, avoiding lost wakeups. Empty
maintenance costs an atomic flag read; refusal backs off without a busy wait.

Module initialization has one admitted module I/O root around foreign entry.
Nested direct Start/Poll/Cancel calls inherit that root, so a genuine pending
continuation is not refused as a different operation. Entry admission and
context-installation failure still enter the same registry rollback path.

This is the backing capability/ACK migration. Direct pending-module cancellation,
complete domain recovery, physical code/frame reclaim, dynamic process capacity,
MMU/heaps and full four-part invocation/accounting remain separate gates. No
arbitrary privileged-fault containment or multi-hart qualification is claimed.

Qualification includes nine focused source-lifetime tests, pinned close-admission
refusal, scoped dependency and post-unlock notification cases, and the broader
683-test host campaign. Native acceptance exercises file-backed sources and
forced reuse, classic archives, all six host-directory scenarios, module
admission including stale-source-layout refusal, process cleanup and malformed
dialog rollback, public Toolbox enumeration, cursor rendering/responsiveness,
application panic recovery, and power restart/shutdown/refusal. The direct
initialization probe exposed and verifies the entry-root correction above.
Private transport coordinates and host disk-space failures were corrected and
the interrupted cases rerun; they were not counted as guest successes.

### Failed module entry and direct-request recovery

Broker admissions distinguish queue-owned calls from direct calls. Each active
callback installs its authenticated receipt in a scoped subject view; a nested
admission validates that parent, its active executor and the original resource
scope. Queued work deliberately starts an independent frame. This records real
protocol ancestry without scanning all requests on each call or inventing IDs.

Failed entry withdraws the module and starts the same invalidate-first owner
walk immediately. Its direct roots have preallocated reply reservations and a
FIFO of genuine receipts. Owned cancellation plans carry the original subject,
provider and buffers. Nested calls remain their parent's responsibility; queued
requests remain with their owning queue. No second coordinator steals them.
Dropping a plan leaves its original request uncertain and pinned. Foreign or
malformed completion refusals return the same plan intact.

Maintenance performs one bounded cancellation or rollback checkpoint. Positive
resource releases coalesce rollback retries; an empty actor does atomic checks
only. Pending work backs off, and unchanged refusals are not logged per poll.
Actual callback results survive temporary registry contention without calling
the provider twice. A proven terminal ACK releases serialized occupancy.

A cancellation can deliver a result for an operation that already committed.
The generic core retains opaque nonempty replies, their original request and
provider pin until class-specific recovery can reconcile those effects. The
failed domain stays inspectable; unrelated service requests may proceed after
the real terminal ACK. Without that reconciliation, full domain retirement is
refused with an informative retained-reply diagnostic. An uncertain invocation
retains its reply reservation and claim, with no guessed ACK. These are explicit
cleanup refusals, not successful recovery or physical code reclamation.

The walk provides resource containment. Arbitrary privileged-fault containment
is **unproven** and is a **required future gate**. Until a recovery contract
exists and passes, the System must not claim it can safely recover from every
mid-call module fault.

Qualification: 31 registry cases, including six new ancestry/cancellation cases,
and the broader 689-test host suite pass. Sixteen distinct native tests pass,
including three failed-entry scenarios: delayed terminal ACK completes the common
rollback, absent ACK keeps the original request pinned, and a committed reply
remains retained. Physical mouse input is acknowledged in all three guests.
Existing module admission, provider/archives/disconnect, process cleanup, panic,
cursor, power and public Toolbox conformance remain verified. The optimized
image, compiled boundaries, formatting and 52-library API documentation pass.
Finder restart and opaque class-result reconciliation remain separate recovery
work; no full isolation or physical-unload claim follows from these results.

### System-owned Finder replacement

Finder's panic report offers **Restart Finder**. The System owns the action and
all report controls; the parked application does not have to run another handler
or issue its own exit request. Standard managed buttons own tracking, highlighting
and cancellation. Report scrolling commits only the report/coverage mask, while a
copy or restart warning commits only its warning bounds and changed controls.

Replacement admission prepares a genuine process owner, saved service context,
complete diagnostic snapshot, distinct image instance and stack before stopping
the old Finder. The immutable package source is shared by an `Arc`; executable
loading still initializes a separate writable instance and applies its relocation
records. Allocation refusal keeps the original owner/report alive, displays the
cause and permits retry. No temporary process is exposed as a running app before
its actual thread identity exists. The app menu suspends prior-owner actions
while a foreground recovery context has no published thread. Physical pointer,
modifier and key state survive saved-context changes; event queues remain private.

Once preflight succeeds, System changes the Finder role to the new incarnation,
stops all threads of the old owner and runs the same cleanup checkpoint/walk as
ordinary application exit. Old saved managers and image reservations remain
reachable through their genuine owner and completion bindings. A refusing or
pending cleanup never frees or reuses that domain. A separate new image/owner may
start once the old threads and stacks actually quiesce, even if old asynchronous
resources remain quarantined. This is a replacement guarantee, not a claim that
uncertain effects have been reconciled.

The recovery controller retains a linear preparation through scheduler refusal.
Its replacement thread stays suspended until process metadata and saved services
are published. A post-handoff start refusal retains the same image/stack and
restored diagnostics for Retry; it does not reissue the image or invent a thread
receipt. Stack waiting advances on actual reclamation notifications; a displayed
retry refusal sleeps until requested. No callback or loader executes under the
controller's lock. Each maintenance turn performs one stage after input service.

Native repeated restart and image-arena refusal/retry qualification pass. The
latter keeps a helper's genuine wide image reservation until its normal exit;
Retry then admits a new Finder instance. Diagnostics remain copyable and their
body pixels unchanged through refusal. Stale window/handle IDs remain rejected
after replacement objects are published, and another app continues receiving
input. This implementation does not activate
page-backed allocations, dynamic scheduler capacity, process isolation or an OOM
emergency kit. The walk provides resource containment. Arbitrary privileged-fault
containment is **unproven** and is a **required future gate**. Until a recovery
contract exists and passes, the System must not claim it can safely recover from
every mid-call module fault.

The heap/reserve campaign retains a concrete additional gate: deliberately
fragmenting the current heap caused an infallible 20 KiB repaint allocation to
abort with about 60 KiB total free. General OOM recovery is not qualified by the
image-admission test; the emergency reserve and fallible repaint paths must close
that case in the heap stage.

Qualification:690 scoped host cases pass (69 native-only gates excluded), and15
native tests pass, including both Finder restart/refusal guests, the six host
provider workflows, module admission, backing sources, original cursor sprites,
complete application panic reports, ordinary/malformed-layout process campaigns,
aggregate public-SDK conformance and external power restart/shutdown/refusal.
Optimized image/structural checks, compiled boundaries, formatting and52-library
API documentation pass. These are the covered native lifecycles; they do not
qualify every possible pending external effect or arbitrary privileged fault.


### Owned thread contexts and native scheduler capacity

The resident scheduler now stores native `THRD` records and `THST` owner backing
through the same registered facility as the other resident managers. Full-width
thread tokens have no classic wire projection or fixed eight-slot namespace.
CPU backend positions retain authentic publication receipts; those receipts can
read the real record after public invalidation but cannot acknowledge cleanup.
Ordinary wake/start lookups still validate the current registered identity.

An owned preparation holds the stack, loaded-image execution/source pins and a
fallibly allocated context. Admission refusal returns that same plan. Actual
stack destruction precedes dropping the image pin. Current-context access holds
an owned read pin; no movable context is exposed through a static reference.
An independent active-context lock prevents completion-before-park checks from
recursively acquiring the scheduler lock. Same-context switches early-out.

The generic walk stops execution and parks each original thread cleanup ticket.
The reaper waits for a noncurrent stack and the last preparation/context reader,
then moves the removal guard outside the scheduler lock. Destruction of the real
stack, code pin and context precedes the facility's ACK. The last reader wakes
reclamation after dropping its strong reference; idle timer ticks do not allocate
or poll retained reader counts while no release has occurred.

Event readiness uses a leased scratch vector reserved at thread admission. A
nested birth can reserve a spare, and returning the RAII lease retains the larger
buffer. No array of eight waiters, per-tick allocation or scheduler guard across
Desktop input routing remains. Process Manager no longer refuses the ninth app.

Qualification passes 858 scoped host cases (74 native-only/environment gates
excluded), the complete focused facility suite, formatting, compiled boundaries,
52-library Rust docs, optimized image/structural checks and 15 native test cases.
Its public SDK campaign runs ten apps together in two successive cohorts, with physical key delivery, fresh native
process/thread/image identities, separate writable globals and handles, then
normal exits and stack-slot reuse. Preemption, cursor correctness, asynchronous
provider cleanup, both endpoint disconnects, Finder recovery/refusal, panic reports and aggregate SDK gates pass.

The optional strict physical-input budget does **not** pass: queued dispatch
maxima were 0.404/0.718 ms (single app/spinner), while decoded physical-key
batches reached 1.323–3.901 ms. A second functional campaign measured queued
maxima 0.285/0.611 ms and physical batches 0.736–3.850 ms. There was no intervening
recomposition in the sampled input-to-delivery trace. Cold emulator paths and
host scheduling remain hypotheses; no baseline attribution or sub-millisecond
physical-delivery guarantee is claimed. Keep that performance gate open.

The current flat heap/image arena still bounds actual admission; this is M-D03's metadata stage,
not page-backed allocation, multi-hart scheduling, MMU activation or isolation.


### Approved configurable Sv39 layout

Chloe approved this initial virtual layout on 2026-10-26. Ranges are exclusive
at the end and describe address reservations, not physical allocations or quotas.
The platform record must validate them once and generate both linker symbols and
Rust constants. No independently maintained copy of these values belongs in
kernel source. Per-process entries retain their own page-table ownership;
M-D02's module and immutable-frame rules remain unchanged.

| Region | Start | Exclusive end |
|---|---|---|
| Application, image preferred at 4 MiB | `0x0000000000400000` | `0x0000002000000000` |
| Per-process supervisor state | `0xffffffc000000000` | `0xffffffc800000000` |
| Module supervisor region | `0xffffffc800000000` | `0xffffffd000000000` |
| System supervisor region | `0xffffffd000000000` | `0xfffffffffffff000` |
| Physmap subrange within System | `0xffffffe000000000` | `0xfffffff000000000` |

The null guard, unused low canonical interval and noncanonical hole remain
unmapped. System's final page stays a guard. The physmap maps discovered RAM
extents only, using large leaves where aligned, and refuses a layout/RAM span
it cannot represent rather than truncating. These selections do not activate
the MMU or the separately approved physical heap/reserve/account budgets.


### Physical pools, stack leases and checked virtual artifacts

`mac-abi::boot::Memory::checked` validates the original borrowed ROM account and
returns an immutable proof. Its free-span iterator subtracts reservations without
copying a second list, sorting RAM pages, or allocating. The redundant model-side
boot representation is removed. The display handoff returns the same proof;
graphical startup also requires the entire current flat image arena to be
reserved. Reserving only System would let a page allocation overwrite a later
app or independently loaded definition.

`mac-kernel-model::bootstrap::BankPlan` places one out-of-line metadata/header/
order-head reservation per usable RAM bank. Layout uses the actual Rust types,
checked size/alignment arithmetic and first-fit contiguous storage. Separate
banks have separate metadata: holes between them consume none. Wholly reserved
banks need no pool. Fragmented storage refuses before native publication and
reports required contiguous bytes and the largest available span. The resulting
free-span iterator subtracts that metadata reservation from the same ROM account.

The native raw-RAM binding is confined to `machine/pages.rs`. It creates exclusive
MaybeUninit views of the reserved bytes; the safe pool constructor initializes
valid enum/Option values exactly once before forming initialized references.
Banks form a permanent reserved linked account without a temporary heap Vec.
Every buddy access, including PageBlock Drop, uses the explicitly selected native
architecture gate before taking its lock. No implicit ungated resident default
exists. The gate masks local interrupts/preemption and restores the prior state,
including nested calls. This remains a one-hart contract.

Prepared app stacks and the idle stack now hold actual page leases, rather than
256 KiB System-heap boxes. Failure preserves the loaded-image preparation and its
typed cause. Existing ThreadCore reader pins and noncurrent-stack retirement
conditions remain the barriers: pages return before the original cleanup ticket
acknowledges physical disposal. No timer-driven allocation or free-page scan is
introduced. The configured QEMU default adopts 8 GiB. The static 10 MiB System
heap, domain charges/budgets, reserve and kernel trap-stack migration remain
later work; page-backed stacks alone do not qualify those gates. The checked
platform minimum is now 256 MiB, with an 8 MiB pinned module-code reservation.
128 MiB is rejected before heap/services because that enlarged reservation and
the required device-tree tail cannot coexist. Page metadata follows the generated
module limit; code capacity does not grant live account or worker admission.

The platform JSON now also owns the approved Sv39 reservations. One checked
record validates canonical halves, page alignment, overlap, preferred image
placement and physmap containment, and emits matching Rust constants/linker
symbols. `mac-riscv-pages` contains the safe Sv39 encoder/decoder and owned-table
hierarchy preparation. Its mappings preserve ownership/permissions, use large
leaves for aligned RAM, early-out without writes on identical leaves, refuse
conflicting/global/shared subtrees and retain partial owned preparation on typed
refusal. Unknown nonzero invalid-entry payloads are preserved. Eight GiB can be
represented by eight 1 GiB leaves without a per-base-page mapping loop.

**Activation remains a separate required gate.** ROM/System still execute in M
mode with identity addressing; SATP is not enabled. These physical Rust references
must never be rebased in place. Permanent mapped views must precede pool/heap
initialization in the supervisor handoff; opaque firmware callbacks, MMIO/DMA
physical addresses, System-stack trap entry, per-process roots/ASIDs and the image
loader must move together. No isolation, privileged-fault recovery, fully charged
allocation or sub-millisecond physical-input claim follows from this slice.

Qualification for this pool/stack milestone: 873 scoped workspace host tests
pass, with 74 native/environment gates excluded; 39 launcher/host/pruning/
user-state Python tests pass, with two platform-specific cases skipped. Sixteen native test functions pass: RAM admission,
ten-app/two-cohort actual stack reuse, cursor, backing sources, Finder restart/
refusal, six host-volume workflows, app panic recovery, two process campaigns,
functional event latency and aggregate public SDK conformance. Native/classic
ECALL smoke, optimized image/structural checks, compiled boundaries, formatting
and 53-library API docs also pass. A serial transport defect that stopped on a
partial refusal prefix is corrected to wait for the complete line; the RAM
campaign was rerun. An extra ABI invocation selected a nested output cache and
hit host ENOSPC; the explicit existing private profile rerun passes.

Physical-input timing remains unqualified: the functional capture records
1.38–2.32 ms decoded-key delivery despite queued-dispatch maxima of 0.280/0.593
ms. This does not close the earlier strict 1 ms failure or establish a cause
for the reported intermittent mouse stutter. Existing fragmented-heap paint
allocation failure remains a heap/reserve-stage gate; this milestone does not
claim general OOM recovery or full original Macintosh compatibility.

### Separate owned CPU snapshots (trap-stack preparation)

Prepared threads and idle execution now own a separate fallible physical page
lease for their saved integer/FP/vector state. The actual VLEN-selected size is
recorded at boot; initial vector state is zeroed and the fixed header includes
the real execution SP. The scheduler copies the incoming complete frame only
when switching away from a runnable thread; same-thread returns add no copy.
The existing ThreadCore/reader and noncurrent-CPU barrier retains CPU state,
stack and image together, disposing CPU/stack storage before the execution pin.
Preparation failures retain the original loaded image and unwind page leases.

That intermediate ownership landing still built temporary frames on the
interrupted stack and did not qualify System-stack entry. The dedicated entry
step below supersedes that bridge and removes its snapshot copy. Domain charging,
mapped guards, MMU activation, S/U enforcement and lazy extension-state saving
remain separate gates.

### One physical admission contract for every entry mode

Graphical and headless native entries validate the same build-defined platform
reservations and complete image arena before publishing a physical pool. The
graphical flag adds the display-scratch loan; it no longer bypasses physical
page ownership in the headless path. Required exclusions may be contained by a
larger validated reservation, without changing the original account or its
free ranges. A missing exclusion reports its exact base/length and the required
ROM/disk rebuild. The ROM and native ABI probe consume admission constants from
the same checked platform record. The probe discovers actual RAM from the reset
DTB and owns its real reserved firmware heap instead of reporting guessed RAM
and an empty reservation list. Its single-bank restriction belongs to that
QEMU-virt probe, not the resident or firmware memory model.

### Dedicated System-stack entry

Native entry now switches to the per-hart System stack before constructing CPU
state directly in the executing thread's owned backing. The intermediate
switch-away copy described above is removed. A permanent scalar control page
keeps partial save/restore detection independent of the interrupted SP. Nested
resident entry validates the System SP before running Rust. Unproven privileged
entry faults report and shut down loudly; they do not attempt arbitrary recovery.

The existing eight-level native service recursion bound has one bootstrap
baseline slot and one refusal slot. Exceeding the service bound refuses before
invocation, restores the caller and reports once per executor. Bootstrap resident
initialization uses a single-use, hart-local stack-entry authority; its baseline
protects live Rust frames across definition calls. Headless startup returns to
its original caller stack; graphical startup starts scheduled execution.

Synchronous I/O now transports executor/subject/resource scope on the same System
stack. Its former fixed256KiB secondary stack is removed. A scoped outer-service
permission admits deferral only while an actual outer ECALL is dispatched; nested
services and timer/cleanup maintenance cannot accidentally park a caller. Retired
callers switch away only at an outer boundary after all resident guards unwind.

The traced private boot emits bounded bootstrap/runtime entry observations; the
canonical capacity gate checks real Rust SP, owned frame and separate app/System
backing. The architecture process gate also submits an ordinary refused ECALL
with SP zero, restoring the app stack before Rust resumes. This checks that entry
does not write an interrupted app stack. Hardware guards, supervisor mapping,
S/U enforcement, domain charges, SMP and lazy vector saving remain later gates.

Native qualification covers sixteen test functions: RAM admission, both process
campaigns, ten simultaneous apps with physical stack/CPU reuse, original cursor
sprites, event delivery under contention, backing-file mounts, both Finder
recovery boots, six host-filesystem workflows, application panic reporting and
aggregate public-SDK conformance. The zero-SP refusal and full FP/vector
preemption cases pass on the real RV64 entry path. The actual runtime Rust SP
is inside the selected 4 MiB System span; its saved frame belongs to an issued
thread and is disjoint from application stacks. Headless native/classic ABI
smoke also passes with the same physical admission contract.

Functional event capture measured queued maxima of 0.302/0.558 ms and decoded
physical-key delivery of 0.720–1.980 ms. The strict 1 ms physical-input gate
remains open; this does not establish the cause of intermittent mouse stutter.


### Native supervisor self-virtualization

The disk ROM and native ABI probe now prepare supervisor-only Sv39 mappings and
admit the actual CPU translation, privilege/delegation, timer and PMP contract
before MRET. The platform explicitly selects QEMU's MMU/PMP/Sv39 properties;
the profile name alone inherits bare defaults. System code is linked at the
checked high virtual address and initialized at its separate physical storage.
The handoff carries physical wire arrays and checked mapping records; fresh
mapped views precede pool, heap or thread owners. No reference is rebased.

The resident builds separate kernel and opaque-firmware roots from real
PageBox leases before heap startup. Intrusive owned table storage needs no
pre-heap Vec, and partial preparation releases every lease. The physical pool's
CPU views use the permanent RAM-only physmap, including discovered high banks;
physical addresses remain allocation/DMA facts. Largest-leaf mapping avoids one
construction operation per base RAM page. Every discovered bank is checked
against the build-defined device windows before pool publication.

The selected 4 MiB System stack has unmapped guard pages in both permanent
roots. Actual entry uses S CSRs and Sstc timing, preserving complete FP/vector
state in separate owned pages. Initial root activation arms hart-local RAII
restoration across every subsequent preparation/refusal and headless return.
The initial boot TLB is retired once; permanent distinct hardware tags avoid
full flushes per sector when available. Tagless hardware requires local fences.
Saved native trap entry already has SIE clear; its checked kernel-root path
early-outs without a redundant interrupt-mask write or restoration object when
the root has not changed. Firmware scopes retain their independent mask guard.
Firmware callbacks execute under masked root scopes with unchanged resident
aliases and bounded copies for application bytes. Opaque firmware code/state
is absent from the kernel root. Trace/observer snapshots read CPU virtual
addresses; framebuffer diagnostics still name physical bus addresses.

The native hardware refusal campaign deliberately reads each System-stack
guard and writes System code: all three fault through the saved-state handler
and terminate loudly. These faults are not recovered. RAM admission verifies
512 MiB/8 GiB boot and informative small-memory refusals; the ABI campaign
executes native/classic public SDK calls through actual Sv39 entry. Capacity
acceptance distinguishes CPU aliases from physical stack/CPU backing and proves
nonoverlap and physical reuse across two cohorts.

This is kernel self-virtualization, not user-process isolation. Application and
module images still use explicit trusted supervisor flat arenas, and application
stacks still use physmap aliases. Private process/image mappings, page-backed
heaps, domain/control-zone charges, recovery kits,
S/U protection and multi-hart synchronization remain required staged work.
TE2/Text Edit migration stays on hold for the replacement common-control plan.

### Native mapping inspection and Reactor Room Memory view

The installed System root is now an inspection source through the public
Toolbox facade. The source reads owned PTE backing, coalesces equivalent
contiguous leaves, and lazily retains immutable mapping metadata. Read-only
batches allocate no caller-owned resident session and repeat capability checks.
Actual stack guards, the heap backing and RAM direct-map aliases are explicit;
shared supervisor arenas and unavailable per-domain accounting are labelled.
The RAM alias is never charged a second time. Heap/page-pool statistics come
from their live backends and do not claim simultaneous global sampling.

Reactor Room switches its single owned window between Domains and System
Memory using resource menus. Each view owns managed controls, released before
admitting the other view, avoiding a second window raster on the current small
heap. Mapping rows are cached by batch; repainting and scrolling do not walk
page tables. Refresh updates changed statistics without rebuilding immutable
rows. Per-process gauges await actual private roots; protected reserve/budget
metrics await the page-backed allocator. There is no synthetic accounting or
periodic ledger sampling on this view.

### Control-paint heap preflight

Managed controls admit immutable title backing fallibly before creation or
configuration is published. Drawing snapshots share those bytes and retain
small appearance values; highlight/value changes and cancellation do not create
another title allocation. Retained older drawings keep their own appearance and
title lifetime. The full native CDEF state is expanded at the existing invocation
binding, keeping the 768-byte wire title out of each retained command's inline
layout. Stateless drawing remains fallible before publication. Paint also handles
an absent snapshot without a resident unwrap/panic.

The public SDK allocation-refusal campaign verifies that a failed title change
preserves both control state and active tracking, that cancellation and existing
title draw preparation work while new allocations are refused, and that retry
succeeds. These checks cover Control Manager admission/snapshot preparation;
they do not claim that font rendering, composition or the complete emergency
service graph is allocation-free. Page-backed/accounted heap activation and its
transitive recovery-kit audit remain separate gates.

### Cursor heap preflight

Animation frames retain their admitted palettes; manual and timer advances select
and borrow the active frame instead of cloning its color vector. Selecting an
already admitted palette reuses its backing. New palette and resource copies are
fallible before cursor-state publication; lookup failures preserve their cause
rather than becoming a missing-resource report. Public SDK refusal tests verify
that animation remains usable after failed replacement and that existing-frame
advances require no allocation. This is an allocator/presentation lifetime check,
not closure of the separately open physical-input latency gate.


### Icon and resource heap preflight

Icon decoding reserves image, pixel, mask, color-table and bundle storage fallibly
before publishing a drawing. Default palettes and the small lookup catalogue use
bounded arrays. Standard 16/32-pixel icon transforms use bounded stack scratch;
larger images admit scratch fallibly before changing destination pixels. Drawing
snapshots share a fallibly admitted family. Refused decode or transform admission
returns NoMemory without a partial icon; retry remains available.

The resource chain is a borrowed iterator. Named lookup, count/index, inline
metadata and Pascal-to-UTF-8 string output do not allocate temporary catalogues or
strings. String output validates both serialized bounds and the full UTF-8 extent
before writing. Type/icon enumeration uses fallible sort/dedup storage and preserves
first-appearance order across the search chain. File-local bundle lookup never
combines one fork's BNDL with another fork's FREF or icon.

Public SDK refusal checks exercise these paths through resident services, including
unchanged pixels/output buffers and retry. This closes these specific admission
paths, not the remaining Picture/Region raster allocations or the complete
recovery graph. The page-backed heap/account/reserve activation gate remains open.


### Picture and bitmap payload admission

PICT command tables, text/comment/polygon operands, color tables, decoded rows
and mapped polygon points reserve fallibly. The extended-v2 recorder admits each
append within its existing byte limit and propagates allocation refusal; it never
publishes a partial recording. Region operands borrow the checked original byte
extent instead of allocating a second serialized copy.

Retained Picture and bitmap drawings keep their single owned payload; unused
deep Clone implementations are removed instead of adding a shared-owner
allocation. Empty pictures require no payload allocation. Public SDK checks
refuse command, operand, pixel and palette admission, preserve destination pixels,
and then retry successfully; malformed input keeps its argument error.

Retained command queues also reserve fallibly before publishing a drawing.
Graphics restores its saved port when queue admission refuses. Legacy command
boxing and retained text/port copies return NoMemory on refused backing. These
entry paths do not silently discard the failed command or replace its cause.

This is payload/queue admission, not a claim that complete replay is
allocation-free. Region clips and algebra, retained port geometry and the full
recovery-service graph remain in the pre-heap fallibility audit.


### Region and replay heap preflight

Empty and rectangular Regions keep their bands/spans inline. Complex immutable
clips share admitted backing, so retained port snapshots do not deep-copy their
geometry. Rectangle clipping uses rectangle arithmetic; complex construction,
Boolean algebra, offset/inset/map and classic serialization report allocation
refusal. Compact classic trap replies write their ten-byte record without a
resident heap buffer. Normalization and original big-endian bytes stay unchanged.

Caret geometry and partial hover bindings are prepared before publication. Owned
plans retain exclusive access through commit; refused scene/geometry admission
leaves the previous visible state. Window exposure scratch refusal retains an
update bounded to the affected object's content. Scene damage capacity is admitted
with its window lifetime, and exhaustion during exact damage coalescing uses the
bounded union for that same object. Healthy updates retain their exact masks and
ordinary occlusion; this is not a full-scene repaint policy.

Retained graphics/control/bitmap/icon/Picture/font replay errors propagate through
the cache admission barrier. Partial back-buffer pixels are not published as valid
cache entries or presented; pending damage remains available for retry. PICT text
and font-name conversion use bounded stack output. This does not establish a
complete recovery policy for every permanent rendering refusal or the full
transitive font/emergency graph; those remain pre-heap qualification work.


### Font scratch and catalog admission

The Font Manager admits its complete family/name/association catalog before moving
font resources from the System fork. Refusal leaves the original resource vector,
order and payload ownership intact. Successful extraction moves payloads without
copying them. Immutable source validity is cached at admission, so width/drawing
requests do not repeat source-format validation. Existing bitmap strikes and the
bundled glyf TrueType faces retain their previous selection and pixels. Unaudited
preferred VARC/CFF/CFF2 sources return NotSupported rather than entering hidden
allocation paths or silently selecting a different outline source.

Synthetic/scaled bitmap pixels, embedded hinting state, supplied glyph scratch,
outline commands, coverage, scan edges/crossings and underline exclusions admit
backing fallibly. Exact unstyled bitmap strikes and validated empty text need no
raster/hinter allocation. Glyph scratch is reused across a string. Small pinned
upstream source patches expose admitted raster storage and fallible embedded
interpreter construction; hint instructions and raster arithmetic are unchanged.
The dependency licenses, original archive checksums and local patch boundaries
are retained with those sources.

Canonical SDK refusal-prefix tests preserve typed NoMemory, single-glyph destination
pixels and retry behavior for real simple and compound TrueType glyphs. Measurement
checks include different glyph scratch sizes. This is not transactional painting
of an arbitrary multi-glyph string, native CFF/VARC/OpenType qualification, installed
emergency-service backing, or recovery from arbitrary privileged faults. Protected
account/reserve enforcement and the complete failure-service kit remain heap-stage
gates.


### System-owned failure-UI formatting

Resource-backed Alert substitution computes its UTF-8 extent before allocation,
and text wrapping borrows paragraph spans. Crash labels use their existing bounded
resource reply storage; diagnostic formatting grows fallibly, and tab expansion
reuses one admitted visible-line buffer. Failure of a full System-owned paint
discards only its own staged transaction, preserves the committed content/hover
policy and restores that window's consumed damage for retry. Partial mask updates
retain their existing cancellation path.

These paths preserve ordinary refusal and retry. They still allocate window,
control, retained drawing and other ordinary backing; fallible formatting alone
is not an installed allocation-free general emergency kit. Resident panic and
infallible-allocation failure now enter the independent terminal display/input
path described below. Protected Save/Alert/Finder backing and all-allocation-
refused qualification of those ordinary services remain explicit heap-stage work.


### Prepared application-menu and focus presentation

Application-menu refresh reuses a second definition state admitted at startup;
owner-group focus reuses geometry storage admitted with its windows. Neither
constructs a new process/window vector during exit. A failed menu measure retains
the accepted snapshot and cancels stale tracking. This does not change the existing
menu presentation capacity or impose a new process-admission count.

Hide Others validates its surviving foreground process before one metadata pass
and one menu refresh. Window hiding batches generation-checked IDs in admitted
geometry scratch. The normal foreground plane requires no Region allocation or
content update; interleaved planes compute exposure once from final survivors.
Scratch refusal conservatively preserves affected-object damage without treating
a bounding box as occluding cover. Repeated/empty changes do no geometry work.


### Original-account execution and resident heap charges

Each admitted process and module owns one canonical account constructed from its
authentic lifetime ledger. The checked platform record selects process kernel
and application caps (initially16 MiB and64 MiB), and a module kernel cap
(initially16 MiB). Future module workers have a checked4 MiB stack policy; this
configuration does not activate workers. Budgets are admission limits, not
physical reservations or guarantees of available memory.

Thread preparation reserves complete buddy-rounded stack and saved CPU-state
extents before requesting pages. Application stacks charge the application side;
saved CPU state charges the kernel side. Both actual thread control-block Arc
allocations and per-owner scheduler/ledger storage use the admitted child's
kernel authority before publication. A DomainId, module number, or copied usage
record cannot select an account. Native allocation borrows the actual active
context's selection without cloning retained readers or allocation authority on
every allocation. Local interrupt/preemption exclusion remains architecture-owned.

Every resident heap block contains its exact backing Layout and original refund
receipts. Admission includes the header and the free-list backend's actual
rounding; alignment padding retained by the free list is not charged. A failed
backend request drops its reservation after releasing the heap lock. Disposal
returns backing before refunding the original account, regardless of which
subject frees it. Reallocation reserves replacement capacity against that same
original account and class, charges both live blocks during copy, and preserves
the original block on refusal. Shrinking within admitted backing retains its
capacity and charge. Ordinary allocation closes when the owner stops. An already
admitted I/O root retains only its original budget-capped continuation authority;
closed accounts always refuse growth.

The permanent parent authority counts ordinary heap occupancy once. Foundation,
System ordinary, and domain-attributed allocations are categories within that
same480 MiB capacity; a domain counter is attribution of the same storage, not
additional physical memory. The32 MiB protected partition is excluded. Authority
control blocks use static **System foundation** receipts, including their actual
headers and backing alignment, avoiding self-account construction and retirement
cycles. Per-domain pool arrays, payloads and thread metadata are not foundation.
Shared directory/index capacities, definition/image sources and retained System
caches have narrow System scopes; drawing callbacks retain their actual subject.

A scoped allocation capability is derived only from a published live/pending
resource root. Root closure and bounded budget reservation share a per-root
guard, with scope-to-account lock order; allocation never reacquires the owner
ledger or invokes backing allocation under those guards. The root's unique owner
closes before release acknowledgement. Retaining a capability cannot keep its
root open, create a root, or reopen ordinary owner/account admission. This is
continuation authority, not reserved physical storage or a general OOM guarantee.

A buffer can outlive the operation which first allocated it. Replacement can use
its original still-accepting account after that old root closes. After ordinary
owner stop, a different surviving root can authorize replacement only through a
genuine capability for the **same canonical account**, checked by actual account
identity rather than DomainId equality. The original class and refund destination
remain unchanged. Budget failure and physical exhaustion never select a different
account or heap. System replacement likewise retains its original static category.

Queued I/O retains its original allocation target alongside genuine operation
and resource authority. Submission buffers, queue payloads and delayed provider
processing reinstall that target instead of inheriting a timer executor's System
selection. Bootstrap I/O preserves selection through its actual context handoff.
An unported architecture returns a typed unsupported result for runtime allocation
selection; it does not substitute host allocator behavior for the native path.

Retirement waits for actual thread quiescence and settled cleanup. It then drops
saved manager payloads and all ready checkpoint references, detaches empty stopped
ledger storage outside its lock, and waits for every original receipt/use before
removing the account. The drained phase never reenters removed managers. The
single empty queue placeholder used while swapping active saved state is System
owned, so it cannot carry one process's account into another process indefinitely.
Failed or pending cleanup retains its real tickets and payloads. Module metadata
uses the same drain-before-account ordering; its original inline name remains
available while final receipts drain.

Public domain inspection labels this partial coverage **execution and resident
heap**. Usage records are atomic observations, not authority. Independent private
application allocators, executable-image physical pages, module runtime binding
storage and complete recovery-service accounting remain outside this coverage.
The bundled filesystem modules now forward heap requests as described below;
bounded native observations and remaining gaps are recorded below. The mapping summary continues to report complete per-domain accounting
as unavailable. Module workers are now implemented as described below. The full
Save/Alert/Finder emergency kit remains gated; a canonical module account or
reserved capacity alone does not make that service kit ready.

Optimized native guest scenarios have exercised process preemption, malformed
launch rollback, Finder account retirement, mapping/resident allocation reads,
module cancellation/rollback, and host-volume read/write/transfer plus abandoned
stream rollback after owner stop. The latest artifact also exercised 24 concurrent
provider-backed fork marks, preserved pool growth, independent offsets and data,
stale-handle refusal, explicit and Drop close, complete root drainage and retry.
That workload does not establish that an allocation originally admitted by a
now-closed operation root was reallocated: the observed fork-pool growth uses
ordinary owner admission. That specific cross-root replacement path remains
unverified. Host model or service tests do not qualify the native allocator,
interrupt, scheduling, teardown or module-worker contracts. Explicit concurrent
issuer/stop races and allocation-refusal retry of shared authority construction
also remain unverified.

### Boot-pinned terminal panic and OOM path

Graphical startup admits a terminal-only kit after permanent roots and physical
pools, before the ordinary global allocator or services start. It owns an
independent scanout frame and a stack using the checked System stack policy
(currently 4 MiB). Static dedicated input rings, display DMA scratch and bounded
screen/cursor/decoder state stay pinned until reset. No terminal framebuffer or
input-ring bytes alias the normal compositor or normal input queues. Preparation
validates DMA translations and device capabilities before publishing the kit.
The checked requirement manifest counts the actual rounded frame and stack
backing plus the static state and DMA extents; `EMERGENCY_KIT_READY` reports that
census with `full_recovery_kit=false`; protected capacity and usage are separate
from dedicated image storage and immutable asset payloads.

The current 800×600 profile needs 1,920,000 visible pixel bytes, page-rounded to
1,921,024 bytes, alongside its 4 MiB terminal stack. Both come from disjoint
subranges of the protected heap, consuming 6,115,328 bytes together. They hold
no additional buddy leases. Dedicated input/DMA/control statics and immutable
assets remain separately pinned in the System image; they are not heap bytes. Other state is measured
from its actual layouts rather than assigned a guessed reserve. The 1,024-byte
diagnostic payload reuses the existing definition-panic bound; record formatting
preserves a whole UTF-8 prefix and flags truncation or formatter failure. Its
storage lives on the terminal stack. The original bomb bitmap, exact Chicago
strike and arrow cursor come from resident bundled assets, without querying the
Font, Resource, Window or Control Managers.

On resident panic, infallible-allocation failure or a privileged trap, a one-shot
terminal claim masks interrupts and switches to that independent stack. It
abandons interrupted execution permanently: no unwinding, scheduler service,
ordinary lock acquisition or resumption of suspended code occurs. Direct bounded
UART writes retain diagnostics without asking the failed allocator for statistics
or consulting execution-context machinery. The safe renderer consumes only its
bounded record and owned pixels. Formatting implementations supplied to this path
must also avoid allocation and service calls; a bounded destination cannot make
arbitrary `Display` implementations safe.

The resident panic handler therefore reads only the panic message's captured
string, when available, and primitive source location fields. It never invokes
the panic payload's `Display` implementation. If Rust retained dynamic formatting
arguments rather than a captured string, the dialog reports that those arguments
were not evaluated and preserves the source location. This deliberately limits
diagnostic detail instead of risking a deadlock before the terminal can draw.
OOM request sizes and CPU trap fields use trusted primitive formatting; definition
panic reports arrive as already-captured bounded text.

The RAM framebuffer handoff uses separately retained, prevalidated DMA state.
Normal configuration and terminal activation arbitrate ownership; uncertain DMA
completion never licenses scratch reuse. Terminal input resets devices before
publishing its separate rings, validates every completion before indexing storage,
and retains failed endpoints' buffers. Restart uses the checked platform reset
register directly, without the normal driver registry or callbacks. Recursive
failure, unavailable terminal backing, or an unusable display/input handoff reports
and requests a loud platform shutdown. These are terminal actions, not recovery
of a privileged call. A native terminal winner is tied to its actual admitted
execution owner. Before exclusive kit access it requires irreversible machine-stop
acknowledgments from every other CPU. A distinct peer loser abandons execution
and waits for actual stop; same-owner recursion reports and shuts down directly.
This contract still requires native S-peer qualification and does not imply
arbitrary privileged-fault containment.

The platform's required `fatal_shutdown` record names its MMIO device and supplies
distinct nonzero `failure_value` and `restart_value` commands. Build validation
checks the device against the mapped inventory and emits the Rust constants and
linker symbols from that same record. Normal graceful Shutdown and Restart still
use the selected external power driver; this direct register path belongs only
to terminal failure handling.

The Restart button supports default-key activation, pointer tracking, cancellation
and release outside. Button updates damage only the button. The cursor saves and
restores its own pixels, and unchanged motion does no pixel work. Host tests run
formatting, drawing, input and cursor behavior with allocation refused. The native
definition-panic test verifies original bomb pixels, bounded cursor/button damage,
cancel restoration, diagnostic contents and an input-driven platform restart into
a second ROM and desktop boot. Private native qualification also passes for a
real Rust panic with the allocator and desktop locks held, followed by pointer
Restart, and an oversized request refused by the actual allocator backend,
followed by allocation-error handling with those locks held and keyboard Restart.
The refusal probe calls the concrete backend directly: an unused global
allocation/deallocation pair can be optimized away and is not exhaustion evidence.
All three scenarios verify unchanged base media and reap their owned guest.
Device-completion rejection, loss of the last input endpoint and cancellation of
an abandoned press have allocation-refusal host coverage through the same safe
logic used by the native driver; physical device-failure injection is not claimed.

This installs one terminal service within the protected 32 MiB heap partition.
It does not complete the emergency-service kit or provide a general recovery
allocator to ordinary callers.
Save/Save As, core alerts, ledger teardown and Finder restart still require their
own measured protected backing and refusal/concurrency qualification. Module
workers now have original-account allocation authority and real stack, CPU and
metadata charges, described below. Arbitrary privileged-fault containment is
**unproven** and is a **required future gate** (M-D04); displaying a terminal
diagnostic and restarting does not satisfy that gate.

### Fixed page-backed System heap

The checked platform record now selects the fixed System heap capacity
(`memory.system_heap_bytes`, initially 512 MiB) and its usable virtual base
(`virtual_memory.system_heap_base`). The limit is derived, not independently
configured. Build validation rejects arithmetic overflow, misalignment,
noncanonical addresses, placement outside System, and collisions with the entire
image reservation, entire guarded System-stack window or physmap. One checked
record emits both linker symbols and resident Rust constants.

After physical pools exist but before permanent roots or ordinary allocation,
bootstrap requests one real buddy lease for that complete capacity. Root
preparation maps the configured usable bytes supervisor read/write, nonexecutable
and global in both the resident and firmware trees. The same owning `Spaces`
record retains the physical lease until reset. No static heap array remains in
the System image. GlobalAlloc is initialized only after the new root is active;
no live allocator reference is moved or rebased.
Heap admission reserves raw storage without clearing the entire 512 MiB: the
free-list allocator initializes its own metadata, and GlobalAlloc's normal
uninitialized-allocation contract applies. Existing stack, CPU and table callers
retain their zeroed-backing path.

For a non-power-of-two capacity, the unique physical lease includes the exact
buddy rounding. That tail is retained until reset, excluded from physical-pool
free counts and never added to allocator capacity. `SYSTEM_HEAP_READY` reports
usable bytes and actual physical backing bytes separately. Inspection classifies
only the actual usable heap PTEs as System heap and validates their physical
offsets and supervisor RW/NX permissions. The direct map remains an alias of RAM,
never an additional charge or evidence that retained padding is available.
Heap buffers have checked CPU-to-bus translation and remain valid across the
resident/firmware root switch, so resident device and firmware I/O need no stale
image-address assumption.

The platform's static minimum RAM is not a heap-capacity guarantee. A 256 MiB or
512 MiB machine can satisfy the fixed ROM/image/module reservation checks and
still fail this 512 MiB contiguous allocation. Refusal preserves the typed page
cause, names the requested capacity, asks for more RAM or a smaller configured
capacity and stops before service/allocator startup. No fallback heap is installed.
Even sufficient total free bytes do not promise a contiguous buddy extent.

The checked `memory.system_recovery_bytes` policy reserves 32 MiB inside this
same 512 MiB backing. The ordinary allocator receives 480 MiB; protected subrange
capabilities are unique, initialized before use and permanently reserved until
reset. The mapping inventory covers all 512 MiB while allocator capacity excludes
the protected portion. Partitioning alone does not install original-account
charges. Resident allocation charging is described above; complete metadata
attribution and private application roots remain unfinished. Module workers now
use original-account backing as described below.
Full recovery-service census and Finder allowances remain open; reserved capacity
alone does not qualify those services.

Qualification covers checked map collisions, full stack-window exclusion,
canonicality, alignment, overflow and capacity-dependent artifact generation.
Native disk boots refuse 64/128 MiB below the placement minimum and 256/512 MiB
at fixed-heap admission, then reach the desktop at 2 GiB and 8 GiB. Public SDK
inspection verifies actual heap PTE permissions, physical offsets, capacity,
guarded stack and unchanged partial process-account coverage. The full native
host-filesystem workflow exercises real device DMA and firmware I/O with the new
resident heap: both forks, writes, recursive transfers, readonly refusals,
cancellation, owner cleanup and an actual cold metadata restart. These checks do
not qualify complete heap accounting, the full recovery-service kit or SMP. The
headless native/classic trap ABI and public Toolbox conformance probe also boots
through the same page-backed heap and owned-root admission path.

### Canonical admission and stopped metadata reclamation

OwnerIndex and shared OwnerRef now hold the same atomic admission authority.
Allocator admission can read that authority without reacquiring a ledger whose
metadata growth caused the allocation. Stopping a domain publishes closure
before stopping its pools under the existing invocation gate. The standalone
namespace issuer remains irreversible once returned. Shared System construction
prepares admission and owner-state backing before committing its sole issuer,
so a backing refusal does not consume an unpublished System namespace.

Process retirement first waits for scheduler quiescence and settled cleanup.
Only an empty stopped ledger may detach its pool/bitmap storage; pending
resources and completion faults refuse with their typed cause. Detached storage
is destroyed outside the owner lock before the account death barrier. The
stopped identity cannot reopen, and its actual manager generations retain their
existing rules.

The resident allocation boundary now consumes this admission authority. The
optimized native scenarios described above exercise actual per-allocation
charges during real manager growth and cleanup; they do not establish every
recursion or concurrency path. Explicit allocation-refusal retry of authority
construction and concurrent issuer/stop races remain unverified.


### Module execution sources and forwarded allocation

Module code now uses the existing registered image source/alias facility. A
module source has an explicit module role, never application ACAP authority.
Source role and genuine owner domain kind must match before publication. The
immutable package resource bytes are System owned; instance metadata and the
validated executable-range list use the module's kernel account. Each entry and
request invocation retains a real source read and execution pin. Its executor
and primary resources belong to the module, while the original admitted request
Subject and allocation target remain separate. Runtime binding metadata retains
no permanent read/account pin that could prevent its own retirement.

Direct Host callbacks authenticate the installed executor before using any copied
context number. Allocation authentication borrows the active context without
cloning retained readers or account authority. Shared image cleanup works from
both process and module owner walks. Stopped module image aliases finish only
after non-image cleanup and real invocation readers have drained, before final
account retirement. Physical module RAM remains reserved until reset; this slice
does not reclaim those image pages.

The checked Host layout installs allocation/free/reallocation callbacks before
author initialization. The bundled Host Filesystem and StuffIt Filesystem no
longer contain private BSS heaps. Their persistent caches and shared capacities
use explicit module allocation scopes; transient request futures, buffers and
lower calls retain their original request target. Successfully quiescent service
state releases its retained capacities outside module locks; refusal preserves
live operations. The zero private-workspace declarations do not create a new
quota or waive the selected 16 MiB module kernel account. Image-only DMA admission
is unchanged: forwarding is not authority to submit arbitrary heap buffers to a
device.

The combined optimized RV64 image exercised real module entry, nested declared
services, package refusal, failed-entry cancellation and final account drainage;
all six existing normal host-filesystem scenarios and Finder recovery passed.
After the final protocol-status diagnostic refinement, the optimized image and
public-SDK host-volume and StuffIt scenarios passed again. These include actual
forwarded allocations during both forks, writes, recursive transfers, nested
archives, decoder/malformed-data cases, stale handles and owner-stop rollback.
The first StuffIt attempt failed before guest execution because of the host Unix
socket path limit; only the private evidence path changed for its retry.

These scenarios do not directly assert module-primary Resource Manager lookups,
forged Host context rejection, all quiesce paths, concurrent stop/pin races, or
execution of the new fatal OOM/protocol diagnostics. Those remain unverified.
Ordinary refusal returns through the service; an infallible module allocation
failure reports its requested layout without inventing its cause. A returned
non-allocation error is preserved as a protocol failure, rather than relabelled
as OOM. The native diagnostic formatter is bounded and allocation-free by design;
its new failure branches still need target execution evidence.

Module workers and their native critical-section contract are described next.
Full protected Save/Alert/Finder recovery services remain an extension-backend
readiness gate. Arbitrary privileged-fault containment remains unproven (M-D04).

### Native module workers and shared-state exclusion

The driver SDK now prepares a captured `Send + 'static` closure as an actual
module-owned scheduler thread. Preparation admits the closure through the
resident forwarding allocator, its configured 4 MiB stack, complete CPU save
storage, context and lifecycle metadata against the module's 16 MiB kernel
account. Shared scheduler capacity remains System foundation. A preparation
refusal returns the original closure to its caller. The shared THRD/THST
facilities issue and validate thread identities; there is no second worker-ID
table or application identity invented for a module.

Preparation is inert. Starting during module initialization only arms the
thread; successful registry publication activates armed threads in the same
excluded transition. Failed initialization cannot run their bodies. Preparing
and starting after publication uses the same checked lifecycle. Captured-state
ownership transfers at the single irreversible THRD publication point, with no
later fallible CREATE step that could falsely return ownership to the caller.

Each worker retains a genuine module image read and execution pin. Its executor,
resource source and kernel allocation account come from the module authority.
A separate admitted DIOP Subject belongs to that worker, independently of its
initializer. Requests started in the worker retain this Subject through Start,
Poll, Cancel and queued completion; queued subjects may outlive exited stack
backing. Deferred capture destruction restores the actual worker Subject and
THRD allocation target before entering the pinned module. Merely moving an
already-pending request from an initializer or another thread does not transfer
its original context: such a continuation is rejected as foreign. A future
context-transfer capability is required to support that operation.

Copied wakers retain only a generation-tagged token. Wake is coalesced;
cancellation is sticky. Park checks wake/cancel atomically with the scheduler
transition and uses the actual native trap/switch boundary. A worker controller
queries a small result record without blocking. Dropping a preparation or
controller requests disposal; it neither waits nor executes a foreign destructor
under the caller's manager locks. Owner teardown wakes cooperative cancellation
and retains its unique cleanup ticket until execution and destruction finish.
Uncooperative privileged code remains retained; forced safe unwinding is not
claimed.

Heavy backing may be detached only after the real FINISHED/EXIT transition,
when non-runnable, noncurrent and free of execution readers. A returned body is
insufficient before EXIT. Stack, CPU storage and image pins need not remain
attached to a finished result handle. Foreign destruction runs only at the
scheduler's guard-free maintenance boundary. A refused cleanup keeps its payload,
pin, ticket and typed cause for retry; it does not panic or silently discard
ownership. Bootstrap selects the actual Finder thread rather than assuming the
first scheduler slot belongs to a process. A future API exposing new readers of
finished rows must also arrange a final-reader disposal wake; today's control
queries return copied snapshots and acquire no such reader.

The safe module `Mutex` runs a bounded synchronous closure inside authenticated
native IRQ/preemption exclusion, tracked on the actual thread. Both acquisition
forms try the lock: contention returns `SERVICE_BUSY` or `None`, never spins
with interrupts disabled. A borrow cannot escape the closure. Park, start/create
and foreign provider entry reject the critical context; bounded wake, cancel,
disposal and allocation remain available. Worker computation and I/O run outside
these short scopes. A plain spinlock shared with a preempted worker is not made
safe by the existence of this API. Multi-hart qualification remains outstanding.

The optimized QemuRISC-V/RVA23 single-hart artifact exercised two real armed
workers alongside Finder, a later parked worker, wake-before-park/coalescing,
running and parked cancellation, actual broker Start/Poll/Cancel continuity,
stale and foreign tokens, nested critical-section refusals, capture destruction,
and stack-budget refusal with the original closure returned. Peer/spare/parked
backing reuse was observed. The normal coordinator's final marker is emitted
during capture destruction before EXIT; it does not directly assert that
coordinator's final physical reap or result-row removal. A separate intentional
initialization-failure run observed zero worker-body entries, four expected
capture drops and the real module retirement/account barrier. Its first attempt
stalled; correcting the bootstrap slot assumption allowed both scenarios to
pass on a rebuilt frozen artifact. One physical Finder input observation during
worker computation establishes functional progress, not a latency bound.

Actual backing bytes are charged; cumulative CPU-time attribution is still
unavailable. These bounded observations do not establish SMP safety, exhaustive
stop/pin races, stress behavior, arbitrary privileged-fault recovery or the full
protected recovery kit. They do not by themselves authorize the complete
extension-backend handoff.


### Native peer idle admission

Normal graphical boot now consumes genuine per-CPU preparations into one-shot
M acceptance, independent warm System initialization and actual startup SSIP ACK.
The shared boot wire is176bytes with explicit BOOT/HART roles and a complete
monitor layout fingerprint, keeping development ABI1. One shared integer/CSR
probe checks each CPU's admitted profile. Warm entry uses its actual permanent
firmware root and guarded service slot, installs the poisoned System vector,
then permanently selects the owned kernel root and canonical idle context.
No reference is rebased and no peer can initialize global services or select the
bootstrap thread as a fallback.

Ready-result metadata is System Foundation storage reserved before any release.
Typed startup refusals retain the original preparation or pending publication,
plus all already-completed owners, through the nonreturning boot refusal. No
reduced-CPU fallback, synthetic ACK or retry over uncertain hardware exists.
Only the primary is scheduling-eligible; admitted peers resume only their own
full captured idle frame. Actual readiness ownership is retained until reset.
A future scheduler-policy consumer must retain that authority and establish its
real generation/shadow/affinity gate before promotion; no unused dispatch grant
or standalone boolean promotion is exposed today.

Terminal ownership uses the actual sealed cold/hot selection, not a hardware
number or shared boolean. Cold panic entry uses no ordinary control lock;
a runtime peer loser waits for its actual machine stop instead of killing the
winner as apparent recursion. A sealed, hart-affine winner revalidates its
selection before requesting irreversible stop. Only after real peer ACKs may it
borrow the separate protected kit and polled input/display state. Same-owner
recursion and failed takeover remain loud terminal failures. No privileged
recovery claim beyond M-D04 is added.

[Threading](../THREADING.md#hart-execution-and-smp-admission) records the exact
native images and bounded admission/returning-publication/worker/account/UI/
terminal-reset observations. These are not cross-CPU author/service safety,
full register preservation, race/latency or arbitrary-fault qualification.
Independent application heaps, private process roots, image-page accounting and
remaining protected recovery services are still staged. Common-control/TE2
migration remains separate.
