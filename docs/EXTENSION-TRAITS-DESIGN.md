# Extension traits and composition: shape for review

Status: design deliverable for memory/app/resource spec §10, 2026-10-23.
**Not a published trait/ABI revision or an implemented extension runtime.**
Inputs: workspace `extension-traits-shape-brief.md` and
`extension-driver-system-spec.md`, [P6](SYSTEM-EXTENSIONS.md),
[Host Clipboard](HOST-CLIPBOARD.md), [P13](PROCESS-ISOLATION.md), and the
[memory model](MEMORY-APP-RESOURCE-MODEL.md). The newer shape brief's invariants
are binding for this proposal. Conflicting older rules are listed below rather
than edited out of their source documents.

## One registry, explicit lifetimes, coarse dispatch

A package declares the contracts/revisions it provides and consumes, rights,
capabilities and dependencies as generated metadata. Validate and stage those
claims before invoking it; publish its registrations atomically only after
successful initialization. Failed staging releases its own ledger entries.
Providers, hooks, settings grants and cached resolved references all use the
central generation-tagged facility, owned by the registering domain incarnation.

The resident registry stores **host-owned dyn-compatible adapters**, reached
through checked registration capabilities. It does not pass a compiler-private
Rust vtable, allocator-owned String/Vec or borrowed fat pointer across separately
linked code resources. Generated contract records and checked C-compatible
request/completion gates form that boundary. Within a linked component, the SDK
provides idiomatic Rust traits; the host adapter implements the corresponding
current service trait. This also preserves a path to other CPUs: descriptors
contain semantic contracts, not RV64 calling registers or instruction addresses
masquerading as authority. Architecture trampolines stay in platform adapters.

A service call validates identity, rights, selected revision and provider state,
then pins the registration/code/resource generation for the invocation. Cache
entries retain handles plus generations and are invalidated at domain retirement;
a cached raw vtable is never usable without that validation/pin. Releasing a pin
can finish deferred teardown, but never resurrect a retired registration.

Version support is required for **published external contracts**, not old
unreleased project images. Use current-revision interior adapters. Older
published contracts have adjacent-revision System wrappers; generated delegation
plus explicit semantic additions/refusals, never silent guesses. A typed missing
capability is legal only when the consumer has a specified degraded behavior.
The landed exact-revision broker is not yet this multi-version feature; do not
claim it is. Publication point and wrapper coverage need explicit review.

Dynamic dispatch happens once per service request, chain link, policy decision,
logical text block, shaped text run or editor operation. Decoding pixels, scanning
bytes, tree walks, filesystem comparisons and raster loops remain statically
dispatched within the implementation. Requests are borrowed validated views or
owned capabilities, not arbitrary pointers a provider may retain.

## Trait families

These names describe shapes, not finalized signatures. All receiver methods use
`&self` or `&mut self`, concrete request/reply types and borrowed lifetimes;
none returns `Self`, has a type-generic method or exposes an unconstrained
associated type. Host stores concrete wrappers as `dyn` at these boundaries.

| Family | Kind and granularity | Implementer / caller | Dyn-compatible shape |
|---|---|---|---|
| Event/Finder participant | Synchronous participant; one event/operation plan | Enabled component / chain owner | `participate(context, borrowed_request) -> Result<Decision, Failure>`; concrete transform schema |
| Event/Finder observer | Queued read-only observer; one committed snapshot or coalesced revision | Enabled component on its thread / mailbox runner | `observe(context, borrowed_snapshot) -> ()`; no decision channel |
| Named service | Registry service; one request or deferred operation | Provider / authorized client | `begin(request) -> Result<Completion, Failure>`, `poll(operation)`, `cancel(operation)`; completion contains owned capability or typed reply |
| Scheduler policy | Invoked service; one scheduling event/decision batch | SCHD component / resident mechanism | `decide(context, runnable_view, events) -> Result<DecisionBatch, Failure>`; preallocated output, no allocation or wait |
| Definition families | Invoked service; one draw/hit/measure/state transition | CDEF/WDEF/MDEF/etc. / corresponding manager | Concrete operation + port/state/buffer capability; returns typed geometry/state/result, no raw app address |
| Device drivers | Invoked/deferred service; class request | DRVR / broker | Existing `mac_driver::Driver` shape: initialize/request/quiesce, borrowed buffers, Complete/Pending, ticket/phase cancellation |
| Filesystem providers | Registry service; catalog/fork/batch request | FSYS provider / resident File Manager | Existing class codec via driver adapter, Begin/Poll/Cancel operation handles; preserve deferred provider subsystem |
| Font engine | Registry service; probe/metrics/shape/raster **run** | Font extension / Font Manager | Concrete font/run view and bounded caller output; typed unsupported glyph/format; no dyn dispatch per pixel |
| Text data source | Pull service; extent query or logical block | App store / TE2 deferred request broker | extent/read-block completion, Pending, generation/revision-bound block; no async trait or runtime object required |
| Resource editor | Invoked/service; inspect/layout/edit/export operation | EDLS-selected Editor package / Res Edit host | Type/schema declaration + resource snapshot + editor session capability, typed patch/preview result |
| Settings/store access | Capability service; snapshot/transaction | Owning service / granted component | Versioned read/commit request with compare-revision and typed conflict; independent from hook rights |

Drivers retain class contracts, source leases, capability discovery, provider
catalog identities, typed deferred errors and cancellation receipts. Do not
replace FSYS with a new private vtable or duplicate on-disk formats. Introduce
native typed adapters around the existing checked codecs; no manager logic
moves into applications as a side effect.

Font Manager retains the declared built-in bitmap service. Engine retirement
invalidates engine leases and generation-keyed caches, then reports the specified
degraded built-in behavior for affected text; it is not an unreported provider
substitution. Future OpenType/color engines and alpha-capable port results fit
these service boundaries without per-glyph/pixel ABI hops. Their rendering and
cache implementations are later work, not deliverables of this shape milestone.

Text sources must not call back into their own control, allocate from its kernel
domain, or stall a paint. The broker holds a generation-bound request, releases
UI locks, requests a logical block and renders Pending. Completion invalidates
only affected chunks/rows. File-backed text stays app-owned; owned TE2 rope stays
control-zone-owned. The API remains UTF-8 text, not public rope internals.

EDLS declares resource types, Editor contract revision and matching format crate/
schema identity. Opening a session pins the selected generation and immutable
resource snapshot. Editing later returns validated patches through the host's
write contract. Editors never receive the ability to mutate a resource fork just
because they can preview it. This phase designs the extension seam, not Res Edit
write-back or a second schema/format implementation.

## Participant algorithm: synchronous and transactional

1. **Register.** Chain owner defines schema, allowed transformations/vetoes,
   granted rights, work budget and ordering policy. Store immutable order key
   `(declared phase/priority, host-issued registration sequence)`; reject invalid
   orders/cycles/rights before publication. Persisted enumeration shows that key.
2. **Admit.** Allocate or reserve a request record under the subject account,
   capture the original input and current chain revision, and take a pin on the
   ordered registration snapshot. A fixed revision prevents concurrent additions
   from changing the middle of this request. No registry/desktop/store lock is
   held across code invocation.
3. **Invoke synchronously.** Before **each** link, validate its generation and
   active status and acquire a host invocation guard. It installs executor/fault
   identity, subject, allocation context and nested module resource scope
   independently. The callback sees immutable input and may return Pass,
   schema-valid Transform, or Veto/Consume. Its changes are proposals only;
   the host performs no externally visible side effects until chain commit.
4. **Validate.** Validate transformed payload and rights before passing it to
   the next link. Veto is an intentional typed result, distinct from provider
   failure. Consume ends only chains which explicitly authorize that semantic.
   A participant returning Pending/WouldBlock is a contract refusal for this
   synchronous chain, not a hidden queued continuation.
5. **Commit.** Revalidate the operation's preconditions/catalog revision and
   commit once through its service. Only then emit the immutable observer
   snapshot. Failure leaves the prior store/UI state intact. The chain cannot
   make external side effects transactional if arbitrary plug-ins perform them;
   participant contracts therefore prohibit independent mutation during planning.
6. **Mid-chain retirement/failure.** A generation that died after snapshot,
   disconnect, invalid reply, typed exception, or detected budget violation fails
   this request with provider/contract/link/cause named. Discard staged transforms,
   release all pins/guards, quarantine by policy, do not execute remaining links
   or retry a non-idempotent operation automatically. A later independent request
   uses a new live snapshot. No silent Pass on death or implicit deadline default.
7. **Unregister.** Stop admission; invalidate the registration through the owner
   ledger; queued/new users fail validation. In-flight pins drain before freeing
   callback state or code. Disable and initialization rollback use this same
   ownership path; no special manager cleanup roll call.

A synchronous callback can consume CPU. Ordinary preemption plus a bounded
contract must keep unrelated UI/threads progressing, but forcibly unwinding an
arbitrary privileged callback holding state is **not proven** today. Synchronous
participant fault containment cannot honestly be declared complete until the
invocation/cancellation mechanism satisfies that gate. No spin timeout on the
host's locked stack, and no unsafe jump over Rust destructors, is proposed.

## Observer algorithm: no veto path

Observers subscribe only to committed, immutable snapshots. Owner queues a
capability/revision, never calls observer code under its lock. Each observer has
a configured accounted queue, FIFO for noncoalescible events; revision notices
may coalesce to latest only when their schema permits it. Full queues use the
schema's explicit drop/coalesce rule, increment a visible counter, and never
block the publishing service. Queue capacities derive from reviewed policy, not
a hidden event-count constant.

The receiving **observer's own thread/context** validates registration/session
again and enters its RAII resource scope before delivery. `observe` has no
Decision return and no reply capability. Transport/endpoint failure accounting
is host-owned; it cannot retroactively veto the committed operation. Late work
from a retired generation is dropped and counted. Cancel/drain queued work,
then release its pins after stopping new admission. An observer wanting a store
write needs a separate granted store capability and performs a new transaction.

## Service requests, errors and runtime independence

Registry selection yields an owned service lease, or a typed refusal naming
contract/revision, provider (or absence), owner and reason. Reasons distinguish
MissingProvider, RevisionMismatch, NotImplemented, Permission, Quota/NoMemory,
Stopping/Stale, InvalidReply, Timeout, Cancelled, Conflict and ProviderFailure,
with source detail retained. Numeric Status exists at the trap/codec boundary;
no prose classification or catch-all MemoryFull/BadArgument replaces identity.

Pending requests have ledger-owned completion/cancellation capabilities.
Start/Poll/Cancel plus a wake registration implement both event executor and
Tokio adapters; no Tokio task, reactor, `async fn` dyn incompatibility, or
compiler-generated Future crosses a module ABI. Borrowed request/output buffers
expire on return, including Pending. Replies use host-managed buffers or
bounded copies; late replies validate request/provider/session generation.
A completion future's Drop cancels or transfers to a resident draining lease;
it does not forget an outstanding protocol ticket. Cancellation failure keeps
state quarantined and reports why rather than freeing buffers in use.

Introspection lists contract/revision, provider owner incarnation, registration
identity, chain kind/order, rights, state, outcome, queue use/drop counts and last
typed failure. Use one report vocabulary: Ready, Degraded, Skipped* (specific
reason), Blocked, Rejected, Failed, Quarantined. "Retired" is a lifetime condition,
not a competing boot-outcome vocabulary. Counters update per change/delivery;
no serial logging on every physical input or registry rebuild on every event.

## Worked example: Host Clipboard augments Scrap

1. Core Scrap works locally. DRVR advertises a compatible host-channel endpoint;
   an INIT in Extensions negotiates a **separate clipboard protocol**, not FSYS
   container files or an assumed VM flag. Absent/disabled bridge yields an honest
   reported skipped/degraded augmentation; local copy/paste remains valid.
2. INIT's domain registers a Scrap-change **observer** and separately requests
   snapshot/publish store grants. Clear/put batching produces one committed
   revision snapshot; current public compatibility generation alone is insufficient.
3. Queued observer coalesces eligible revision notices on its thread, takes a
   consistent UTF-8 snapshot and sends through deferred channel I/O. No host
   operation enters the input or Scrap lock path. Echo tokens contain origin,
   bridge-session generation and revision.
4. Incoming offer fetch/conversion runs outside Scrap. Publish is a compare-
   revision transaction: reject stale session/conflict or preserve existing
   flavors on unsupported format/conversion failure. Host-imported origin cannot
   re-export as a new guest mutation. Simultaneous updates have explicit revision
   conflict results, not timing-dependent partial stores.
5. Disconnect/retirement invalidates grants, observer and request handles through
   the domain ledger. Already queued old-generation work cannot publish. Pending
   I/O drains before code/state reclamation. Reconnect establishes a new session.
6. Prove real Unicode/emoji copy both directions with Linux **and native Windows**
   hosts, no echo, empty/large/error/timeout/reconnect and extension-disabled cases.
   Mock endpoints alone cannot establish this milestone.

## Worked example: scheduler policy seam

Resident core owns CPU/hart bring-up, timer acknowledgement, context and lazy
vector saves, System-stack switching, lifetime validation and a simple fair
single-hart preemptive bootstrap scheduler. Runnable state belongs to the core;
policy sees generation-tagged thread references and topology, not TCB pointers.

A SCHD INIT depends only on irreducible services. Its bounded `decide` call
receives scheduling events and a runnable view, writes a preallocated decision
batch, and returns. Core validates generation, runnable status, target CPU and
single-dispatch ownership, then performs switches. Multi-hart selection, affinity
and priorities live in the policy; CPU-specific IPI/TLB/context operations remain
resident architecture mechanisms. No dyn calls per register save or queue element.

At a safe boundary, stage against a snapshot, validate a shadow decision, then
atomically publish the active policy generation. Resident watchdog validates
progress/deadlines/decisions and records a typed quarantine on controlled failure.
Fallback parks/drains secondary CPUs at safe points before resuming single-hart
bootstrap; never schedules a thread simultaneously on both. This needs a real
handoff/quiescence test, not a boolean policy flag. Arbitrary privileged corruption
remains a System fault; preemption/deadline containment must be proven for the
scheduler's actual interrupt/lock context.

Missing extension still boots a usable single-hart session and reports the
missing/degraded policy. Implement this **after** the full memory model passes,
in its own isolated capstone commit. The current scheduler stays unchanged.

## Worked example: Finder open-operation chain

Finder offers a public in-app OpenItem participant chain: immutable file identity,
type/creator, mount rights and proposed Launch/Mount/View operation. A component
may synchronously propose a schema-valid target rewrite or a typed refusal; it
cannot secretly move/delete a file or call back into the same chain. Host checks
file revision and capability before final launch/mount through Toolbox services.

For example a package transforms a recognized document open into an editor-open
plan using a compatible installed editor capability. A second policy link may
veto with a named reason. If either registration dies, fail the pending open
without changing selection or leaving a drag/session lock. A subsequent click
can retry against the live registry. Committed open notifications go to queued
observers on their own execution threads, so logging/indexing cannot block open.
No hook re-enumerates folders, remeasures labels, or requests whole-window paint.
Finder uses the Toolbox facade; the hook does not import private System managers.

## Divergences requiring review

| ID | Conflict | Proposed handling |
|---|---|---|
| T-D01 | Older §5.5 defers participant replies/defaults; new brief requires synchronous participants | **Ratified, Chloe 2026-10-23:** synchronous participants; the old queued/deadline/default-on-expiry rule is superseded. Queued rights are observers/services |
| T-D02 | Older §5.2 queues all extension code and bans host-stack callbacks | Synchronous participants and bounded SCHD calls are explicit exceptions needed by newer brief; observer deliveries remain queued. Review invocation-stack/fault mechanism before implementation |
| T-D03 | Brief promises fault containment; landed panic-abort privileged modules are System-fatal | Typed/endpoint failures containable; arbitrary kernel panic/write is not. Stronger invocation gate remains an acceptance blocker, not a claim supplied by RAII alone |
| T-D04 | Registry described as holding module dyn objects/vtables; independent binaries have no stable Rust trait-object ABI | Registry holds host-owned dyn adapters over generated checked ABI; same semantic trait, no Rust-vtable wire contract |
| T-D05 | Published multi-version wrappers vs current exact revision selection and unreleased rebuild policy | Publication boundary + adapters remain required module work. No wrappers solely for stale project artifacts; don't claim published driver support until tests prove it |
| T-D06 | Older teardown is an ordered registration/capability list; new model requires pure ledger composition | Stop admission globally, invalidate per ledger, kind-specific cleanup/cancellation outside locks, wait for quiescence; don't copy the old manual ordered roll call |
| T-D07 | Older pinned-until-reboot modules vs domain-based code reclaim | Logical retirement can land first; physical reclaim only after zero active pins/work. No unload claim before that proof |

## Invariant coverage and review gates

| Brief invariant | Mechanism / evidence still required |
|---|---|
| Coarse dyn + object safety | Concrete request methods, host wrappers; compile-check dyn use per published revision, benchmark boundaries |
| Unmixable chain kinds | Distinct participant Decision vs observer `()`/mailbox surface, distinct registration rights; test observer cannot obtain reply capability |
| Failure containment | Transactional plans, generation pin/validation, cancellation and quarantine; resource containment ratified; **T-D03 privileged-fault recovery proof outstanding** |
| Generation scope | Central owner ledger, per-use checks, retirement cache drain; race retirement against each chain/request phase |
| Enumeration | Registry records include owner/order/rights/outcome, no orphan hidden callback registry |
| One outcome vocabulary | Common report schema above; lifetime state kept separate |
| Runtime independence | Completion capabilities and wake/poll adapters; test event-only and std executor, Tokio integration when platform std is live |
| Typed informative failures | Contract/provider/reason preserved through host adapter and trap; assert unhappy-path causes, rollback and retry |

Design review precedes freezing signatures, contract revisions or capacities.
None of these notes alone establishes hot unload, multi-version compatibility,
async substrate, fault isolation or fully functional extensions.
