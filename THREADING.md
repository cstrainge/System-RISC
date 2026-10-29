# Threading and ownership contract

Every application must execute on its own **OS-scheduled thread**. The kernel
scheduler owns runnable, blocked and terminated OS threads and supplies execution
time independently of calls to `WaitNextEvent`, `EventAvail`, Toolbox managers or
legacy yield functions. Finder is an application and follows this rule too.

Classic Thread Manager contexts are cooperative execution contexts **inside one
application**, hosted by its OS thread. `YieldToThread` selects only contexts of
that application; it cannot schedule another application or suppress kernel
preemption. Classic callback and A5/low-memory conventions are compatibility
state attached to the application/context, not global scheduler state. Native
module workers use the separate contract below. General native application-thread
creation remains open.

The first native implementation now schedules Finder and separately loaded APPLs
on one RV64 hart. Supervisor timer interrupts preempt application and admitted
module-worker code independently of Toolbox calls. Registered, fallibly grown
application thread records own independent page-backed stacks (4 MiB by default,
selected by the checked platform policy within 64 KiB–8 MiB),
separately page-backed saved integer/FP/vector state (storage scales with hardware
VLEN) and
trusted AppId/ThreadId contexts. Native entry uses a separate 4 MiB System stack
before saving CPU state; the same stack serves bootstrap and synchronous I/O
under scoped executor/subject identity. Sv39 maps that System stack between
unmapped guard pages; application stacks still use trusted physmap aliases.
Kernel traps mask
interrupts, including nested definition calls, so allocator and manager guards
cannot be preempted by another application on this hart. Native event waits now
block the actual application thread, with a separate idle WFI context and 250 us
timer sampling. Readiness is inspected before taking the scheduler lock; a fresh
foreground input wake takes priority over background and timeout wakes. No manager
guard or borrowed request crosses the context switch. General application-worker
APIs, blocking primitives, migration and multiple harts remain future work. The
privileged module-worker backend is described below. The dynamic capacity SDK gate now runs ten apps together, delivers physical keys to all nine
workers and repeats with fresh identities after actual exits and slot reuse. A private
RV64 SDK gate has demonstrated another app progressing during a
no-trap/no-yield interval, plus launch, relaunch, ownership, input, hide and exit.

Each scheduling boundary services physical input, cursor presentation and event
wakes before background jobs, provider continuations, document delivery and
reclamation. Fresh input may take one input-only turn; continuous input admits
maintenance on alternate turns to retain I/O and cleanup progress. This priority
does not preempt an in-flight interrupt-masked System call. Such calls must still
bound their work; application scheduling priority cannot repair an unnecessary
whole-window paint inside a service. Held title-bar movement is perimeter-bounded
frontbuffer XOR, with no scene composition, full-screen mask or visibility walk;
release commits the real window geometry. Scene damage under a held outline
copies only its damage and restores intersecting overlay pixels.
The consolidated regression gate also passes the paired Pascal process traps,
twelve caller-posted foreign-window event-isolation cases, and actual Finder/two-
app graphical launch, focus, hide/restore, quit and relaunch tests.

Window BeginUpdate now stages commands and port state until EndUpdate. Another
application can poll/present while that update is preempted and still sees the
previous complete scene. Application activation raises the window plane and
selects its final window or desktop in one transition. Masked content updates use
the same private staging, with RAII cancellation and commit restricted to an
exact local region. Drawing fragments, cache damage and TextEdit cursor regions
all obey that mask; an icon update cannot erase an unrelated editor's hover
policy. Finder-owned desktop masks use screen coordinates, retain transparency
and publish only exposed spans beneath window structures. Desktop feedback uses
the same temporary save-under overlay as window feedback. Preparation and budget
validation precede publication. Idle polling avoids
unnecessary foreground-state swaps; event queues are boxed so real swaps exchange
ownership without copying the arrays. Native apps use `wait_event(mask, ticks)`;
nonblocking NextEvent and legacy interfaces retain their existing behavior. A
bounded 2 MiB pixel cache accelerates committed desktop/window scenes while
preserving original replay on misses. Cold composition and other synchronous
services still mask interrupts. See
[event tracing](docs/EVENT-TRACING.md) for the measured scenarios and limits.

Finder catalog snapshots are application-owned, bounded to1024 items per open
folder. Scan cursors bind the fill to a resident catalog revision; stale partial
snapshots restart before publication. Directory watches invalidate only the
affected folder. Snapshot locks cover local memory only and never cross traps
or event waits. Bulk item storage lives in zero-initialized application memory,
outside resource payloads and the application stack. Selection uses one set/diff
path, cached text dimensions and a row index. Marquee outlines share the cursor's
temporary presentation path and never accumulate retained paint history.

Sv39 is active in S mode, but applications still share trusted flat code
arenas and resident mappings. There is no U-mode process protection boundary. Owner checks prevent accidental cross-application object access through
services; they cannot constrain arbitrary application pointers or machine code.

[P13](docs/PROCESS-ISOLATION.md) tracks the replacement with protected app address
spaces, user-mode execution, checked syscall copies and fault-local cleanup.
[P11 CLAPs](docs/TERMINAL.md) have their own process/OS-thread identities inside a
Terminal session; they never execute on the Terminal app's UI thread. P6/P14
extensions and drivers intentionally remain trusted kernel code.

## Native SDK mobility

Toolbox clients, manager views and object owners are deliberately `Send + Sync`.
They contain routing views and validated object identities, not references into
resident mutable state. Shared services synchronize each operation and derive
the caller's AppId/ThreadId at entry. Copying or moving a client does not transfer
object ownership between applications. Operations requiring a particular OS
thread still report the typed `Error::WrongThread`; movement is not an exemption.
Manager methods taking Copy `self` do not express exclusive resident access;
the System guard supplies that serialization. The public SDK concurrency test
exercises actual shared Memory Manager reads, writes, allocations and cleanup.
This contract does not claim UI worker threads or SMP are implemented.

## Execution identities

The scheduler must supply a trusted execution context at trap entry:

| Identity | Owns or selects |
| --- | --- |
| `AppId` | Resource chain/current resource file, Memory Manager partition and errors, open-file ownership, events, windows/controls, ports/fonts/text state, callbacks and cleanup |
| `ThreadId` | OS stack/register state, blocking/wakeup/cancellation, trap depth and callback stack, error state explicitly specified per thread |
| Module owner | Authentic module ledger, immutable module resources/code and canonical Kernel account; a module worker has its own ThreadId and is not an application or bootstrap executor |
| Legacy cooperative context (planned) | Optional Thread Manager register/stack/A5 state inside one application; never substitutes for ThreadId |
| `HartId` | Hardware trap/interrupt stack and current ThreadId pointer; not the identity of an application |

Request payloads cannot impersonate these identities. The scheduler/service gate
supplies them. Native service-table capabilities are routing authority, not a
mutable global “current application.” Before multiple apps launch, every
application-owned object needs an owner and generation; IDs and cleanup must
reject cross-application access. If a classic API specifies application-global
rather than thread-local state, concurrent calls still synchronize that state;
its historical implicit global is selected through AppId.

## Native module workers

`mac-driver-alloc::Workers` prepares privileged module workers with a safe owned
`FnOnce(WorkerContext) -> Result<()> + Send + 'static` body. They use the same
registered THRD/THST store, native timer scheduler and complete integer/FP/vector
save path as application threads. A worker's execution is typed as Module; it
does not receive a fabricated AppId. Its canonical module owner/account and
immutable module image establish its default allocation, resources and executor
identity. An actual `ModuleExecution` retains the source read and execution gate
through code-dependent destruction and stack quiescence. Native Host callbacks
authenticate the installed module frame or worker baseline; a copied numeric
module label cannot construct that authority. Application-only traps reject
module-worker execution, while the diagnostic LOG operation remains available.

The QemuRISC-V policy defaults to a 4 MiB stack and 16 MiB Kernel budget per
module, with zero Application-class allowance. Page-rounded stack backing,
complete saved CPU backing, Context/Core metadata and forwarded captured closure
storage consume that original module Kernel account. Shared scheduler capacity
belongs to System. These are shared per-module limits, not a separate 16 MiB
allowance for each worker. The module's flat code arena remains reserved until
reset; execution pins do not establish physical code-page reclamation.

Preparation preflights fallible backing before publication. Failed CREATE leaves
the SDK owning its original body; successful THRD publication is the single
capture-ownership handoff, with no later CREATE failure. A prepared worker is
inert. START during initialization only arms it; successful module commit
activates armed workers without allocation. Failed initialization cannot make
those workers runnable. Bootstrap selects the actual runnable Finder process,
independently of which module preparations occupied earlier scheduler slots.

Wakeups coalesce into one pending wake. PARK consumes wake/cancel state under
the scheduler lock before sleeping, and cancellation is sticky. The non-Send,
non-Clone WorkerContext belongs to the actual running worker. Its copied waker
retains a stale-checked token rather than an account or resident core. PARK and
EXIT use the real private native trap/switch boundary; direct Host callbacks
cannot switch an arbitrary borrowed stack. Parking requires an outermost worker
body, interrupts enabled before trap entry and no native critical section. A
nested module callback cannot borrow an interrupted worker's CURRENT/PARK
identity. Native EXIT follows body and capture destruction; it is not a safe API
for abandoning live captures.

The authenticated native worker context also provides
`monotonic_microseconds() -> Result<u64>` and
`park_for(core::time::Duration) -> Result<()>`. Positive fractional microseconds
round up, while conversion or native deadline overflow refuses. A zero duration
validates context and consumes cancellation/coalesced wake without promising a
yield. Wake and timeout both return success; callers check their own monotonic
schedule. Cancellation remains sticky. Clock and park refuse inside a native
critical section. Deadline expiration only marks the canonical worker row
runnable under SCHEDULER; existing `commit_resume` alone patches its return frame.
The native QEMU adapter uses its actual 10 MHz timer policy; this is not a
portable frequency guarantee.

A bounded optimized one-hart native diagnostic run observed a 30 ms deadline
(32,180 guest microseconds), zero, overflow refusal, coalesced wake, early timed
wake and running/parked cancellation. Publication/ACK correlation observed all
four normal and all three rollback worker reclamations. These are scenario
observations, not SMP, latency, pressure or complete extension-shutdown acceptance.


`mac-driver::sync::Mutex` acquires its data lock inside an authenticated native
critical-section thunk. Interrupt exclusion begins before bounded try-lock;
contention returns SERVICE_BUSY (`try_with` returns None) without spinning behind
a preempted peer. No guard or borrowed mutable state escapes the closure.
Critical nesting belongs to the actual ThreadContext and is restored through
RAII, including nested entries. Foreign broker entry and parking are refused
inside that scope. Allocator callbacks and bounded worker wake, cancel, status
and disposal intent remain permitted, with their independent authority checks.
This is not a blocking mutex or a reason to mask interrupts across worker
computation or I/O.

Each preparation admits its own original module-owned DIOP Subject. Worker
Start/Poll/Cancel calls and queued work retain that durable I/O root, independently
of the initializer's request and of the worker's THRD allocation root. Nested
invocations restore their previous selections. Ordinary owner stop does not
revoke an already admitted open-root continuation; closed roots and closed
accounts still refuse. Deferred capture destruction restores the worker's actual
DIOP Subject, original THRD allocation target and pinned module resources/code.
Queued Subject clones may retain the DIOP after the worker's CPU/stack is freed.
Moving an already-pending raw broker ticket from an initializer or another thread
into a worker does not transfer its original Subject: continuation is rejected
as foreign. A genuine retained context-transfer API for that case remains open;
there is no fresh-root or System fallback.

Cancellation and DISPOSE record intent without invoking capture destructors,
including inside a native critical section. A running worker must return
cooperatively; an owner walk retains its unique THRD ticket and wakes cancellation.
It does not force-unwind privileged code. Inert captures are destroyed later under
their pinned invocation. Worker destructors run only from the scheduler's
manager-guard-free maintenance boundary, outside registry/scheduler/owner locks.
A refused scope or destructor retains the payload, code pin and exact cleanup
ticket for retry. Persistent uncooperative execution or refusal prevents module
retirement.

Physical reap requires returned plus FINISHED at actual EXIT (or inert
cancellation), not runnable, noncurrent and no outstanding Core/Context readers.
Returning from the body alone is insufficient: a timer may interrupt before EXIT.
CPU/stack backing is released before its original refund and before the final
code pin. A controller can retain a small named result row without retaining a
4 MiB stack; final disposal and the owner walk still use that row's exact THRD
identity/ticket. Existing readers must leave before heavy reap, and current
getters can no longer acquire a finished context afterward. A future by-ID
Core/Context reader or enumeration API must preserve a result-disposal wake
obligation after heavy backing is gone; a heavy-reap-only pending count would
then be insufficient. No such reader producer exists in the current backend.

Bounded native observations on 2026-10-28 used the optimized RV64 QEMU image on
one hart with 8 GiB RAM, RVA23 CPU profile and 128-bit VLEN. The packaged System
resource SHA-256 was
`211c830b3a9c90df8e5f525aafb34b6870d4a42e324f670d29bd223d1f0fac7c`.
Real ModuleWorkers/ModuleEcho packages exercised two initially armed workers,
a later parked worker, timer progress with Finder and physical input, coalesced
wake, running/parked cancellation, bounded mutex refusal, broker Start/Poll/Cancel,
stale/foreign tokens, capture ownership and the fourth 4 MiB preparation's module
budget refusal. A separate failed-initializer package observed zero worker entries,
four capture drops and actual module rollback completion. Native lifecycle,
mapping inventory, Finder recovery and process scenarios also exercised the
surrounding application paths. These observations qualify those scenarios only;
host-linked services and compilation do not establish target execution.

The normal coordinator PASS marker is emitted by its capture Drop before native
EXIT; it is not final-reap evidence. Separate native reclamation observations
confirmed actual backing, record and temporary-scope disposal plus accepted
canonical THRD acknowledgement for all four published workers, including that
coordinator. The rollback run confirmed reclamation of all three published
workers before module rollback completion. A refused canonical acknowledgement
remains quarantined and never emits a successful reclamation observation.

The same System artifact also ran 44 independently linked modules whose real
workers each completed strict Start-only registration and Status exchanges with
complete replies and cleared broker tickets. That scoped workload does not
establish all-worker teardown, heap exhaustion or UI responsiveness. Earlier
runs exposed broker delivery/reply contention defects; the scoped rerun does not
erase the separately observed Finder ServiceBusy under greedy worker traffic.
Sustained pressure/physical OOM, all cleanup failures, arbitrary
privileged faults, full recovery-kit guarantees, SMP and application std-thread
services remain unqualified. CPU-time introspection is unavailable; scheduler
switch counts are not CPU time. These prerequisites support external extension
development; they do not establish production clipboard sharing or a complete
extension service.

## Hart execution and SMP admission

The architecture selects execution state from this CPU's actual admitted
`sscratch` control. Active context, current scheduler position, synchronous I/O
selection and idle stack/CPU backing belong to that hart record. Runtime lookup
never falls back to a frozen boot hart number. Only the primary record is
admitted today; ROM secondaries remain parked.

Cold bootstrap publishes an allocation-free control after checked BootInfo and
the one-shot System-entry claim. It cannot serve traps: its entering flag is
poisoned until real stack/CPU backing and the prepared control exist. This trusts
the ROM's primary-only entry contract; it is not independent authentication of
arbitrary secondary calls to System entry. Raw CSR/control access stays in the
architecture binding, and safe concrete selection operations expose no mutable
hart borrow across callbacks or nested ECALLs. Unchanged active contexts avoid
Arc operations; allocator reservations still borrow their genuine selection.

Opaque firmware retains its checked fixed owner. A foreign/unadmitted hart
cannot borrow BOOT_THREAD or its default System allocation authority. Fallible
authentication, allocation and firmware operations refuse; owned file cleanup
returns its original argument through both handoff and manager refusal. Invalid
infallible execution selection reports through the independent UART path and
requests terminal shutdown rather than silently spinning.

Each canonical THRD row carries a persistent claim by a sealed hart identity.
The scheduler preflights backing and commits the incoming claim before publishing
its CPU frame and local selection. Ownership spans frame restoration, the body
and synchronous/nested services. Only a checked complete outer trap capture may
hand it off: the native ABI receives a raw frame pointer, all safe mutable frame
and service borrows end, the replacement selection/frame is published, then the
old claim releases under the scheduler lock. Returning from a Rust scope does
not release execution ownership. Both reapers exclude every claimed row,
independently of their caller's hart, retaining the existing reader/ticket checks
and signaling pending disposal on actual claim release.

Cancellation records intent instead of patching another execution's restore
storage. Park return status settles at the scheduler's restore commitment while
it owns the captured or unclaimed frame. A persistent resume-committed bit
distinguishes never-resumed preparations from a selected trampoline preempted
before its body starts. The latter skips a cancelled body, retains the accepted
capture for deferred destruction, and remains runnable through real private EXIT.
It cannot be marked inert merely because its current claim was released by an
earlier preemption.

The shared heap, budgets, accounts, ledgers and buddy pools already have atomic
locks; local interrupt masking is not their cross-hart exclusion. Do not add a
non-reentrant global lock to the invocation gate: owner-ledger growth can allocate
and enter that gate again. Concurrent scheduling and reclamation still require
target qualification, not inference from source locks or a host suite.

Bounded native observations used optimized no_std RV64 QEMU, one hart, 8 GiB,
800×600 and no event/text-input trace. System resource SHA-256:
`32152e1b394176d060a41899522ec389c709bd60c02c5c6265b16a0b0171b3e9`.
Existing native worker assertions completed wake/park/cancel, original-Subject
I/O, stale/foreign tokens, budget refusal and all four normal/all three rollback
canonical reclamations. The strict broker workload completed all 44 clients;
Finder ServiceBusy under that greedy load remains unresolved. Independent SDK
processes completed preemption, launch/refusal, ownership, resource/window/event
and document-lifetime assertions. A real Finder/Cursor Test capture recorded 39
graphic states; 200 paced moves left compositions at 16 and pointer-only motion
left base pixels identical with compositions at 17. These are bounded functional
and repaint observations, not event-latency or multi-hart evidence. Exact remote
cancellation interleavings and full OOM recovery remain unqualified. **M-D04:**
the walk provides resource containment. Arbitrary privileged-fault containment
is **unproven** and is a **required future gate**.

BootInfo now carries the sorted, checked enabled hardware CPU IDs in reserved
firmware data. Structural DT preflight precedes allocation; duplicates, an
absent primary and malformed register widths refuse boot with their exact cause.
The native header must have the current exact size before System borrows it;
rebuild ROM and System together. Discovery does not grant execution authority.
Optimized trace-disabled native ROM/System/Finder disk boots with 1, 2 and 4
QEMU CPUs reported matching topology counts. Secondary PCs matched the ROM park
and their `satp` remained zero; current privilege was not directly observable.
Malformed external DTBs and a stale ROM exercised production boot refusals.
These observations do not qualify sparse hardware IDs or SMP execution.

The QemuRISC-V platform defines four physical harts by default. The normal
launcher reads that platform value; `start --harts COUNT` overrides it. Only
the primary executes System code. The other three remain parked until the core
admission gates pass, regardless of their presence in the discovered topology.
Diagnostic scenarios may explicitly select a smaller physical CPU count.

The custom ROM now supplies a mandatory checked monitor directory. Its per-hart
integer-only M handler uses separate permanent control/stack backing, with no
Rust callback, allocation, FP/vector instruction or ordinary service lock. DT
CPU-controller phandles and paired CLINT interrupt routes supply MSIP addresses;
frequency comes from the checked DTB. Unknown bindings/translations refuse.
The platform's explicit reset contract is whole-machine reset, not CPU-only
restart or hotplug. No secondary S release API exists.

Executable writers order stores, fence locally and await each parked receiver's
matching instruction-fence ACK before publishing an entry. Refused or uncertain
transport retains typed identity through native code loading; an unacknowledged
command retires the command channel until reset. Terminal stop has independent
sticky request/ACK cells, bypasses normal locks and precedes mutable emergency
kit access. Missing stop ACK causes direct fatal shutdown. These mechanisms do
not establish concurrent System execution or arbitrary privileged-fault containment.

The optimized trace-disabled no_std RV64 image with System resource SHA-256
`36735139a17751852a16673d95e666f85cb1eb2763158faa16d3785a31eeda41`
booted production ROM/System/Finder with 1, 2 and 4 physical QEMU CPUs. Physical
control snapshots observed every peer joined, with request8=ACK8, FenceI result0,
`satp`0 and PCs at the actual ROM WFI/JAL loop. Direct privilege was unavailable;
the primary's returning M-interrupt path was not exercised. Wrong CLINT causes,
duplicate controller phandles, an absent physical CPU and a stale160byte ROM
handoff all refused with named causes and no desktop during bounded observation.
A separate private package replaced only the Static Text CDEF with the existing
native panic probe, retaining the shipping System executable. At terminal
readiness all three peers had stop1/ACK1; actual restart booted a second desktop
with fresh joined receivers, successful fence ACKs and clear stop cells. These
are bounded parked-peer observations, not timing, after-boot lost-ACK retention,
concurrent S execution or arbitrary-fault recovery qualification.

Every checked discovered CPU now has its own 4 MiB System service stack, two
unmapped guards, baseline saved CPU storage and entry-control row. Both permanent
roots map all service slots before first activation. Placement uses topology
ordinal and the generated platform stride, never a sparse hardware ID as an
array index. The complete span must fit the configured window; allocation and
geometry refusals do not install a smaller table. Published mapping/backing
owners stay pinned until reset, including for CPUs that never execute S code.

Runtime selection authenticates `sscratch` against the logical raw control
arena before loading its local binding, then checks the permanent row and
online phase in constant time. No typed shared borrow covers assembly-mutated
control words. The primary alone is online; peer controls remain poisoned.
Cold primary admission is explicit and cannot become a runtime fallback.

Unique Foundation receipts attribute rounded physical service/CPU/control
backing separately from heap occupancy. Inspection reports this partial scope,
not all System memory. Its lazily built immutable mapping cache is ordinary
System-owned storage regardless of the first querying application; it cannot
retain that application's allocation account or borrow its recovery permit.
Every prepared CPU also owns a separate idle executor sized by the configurable
System stack policy (currently4 MiB), its saved CPU backing and a genuine THRD
publication in the existing shared thread store. Service entry never overwrites
the idle executor's live stack. Both idle backing owners carry physical
Foundation receipts; private context/record metadata uses System Foundation
heap admission without borrowing a recovery permit.

System idle has an explicit System owner, native thread identity, resource
selection and ordinary System allocation default. It does not impersonate the
bootstrap thread or carry a dummy account or application image. A sealed
publication binds the fresh context to its actual native THRD; all selection
stacks use that identity. Each non-cloneable affine target remains retained by
the actual store/core and installed hart, while scheduled positions resolve
only scheduled records. Primary idle handoff installs this real context.

An unexpected idle teardown returns the original cleanup ticket as a retained
failure; reset-lifetime execution is not acknowledged as destroyed. Failed
installation retains its actual idle owner through the typed boot-refusal
report and nonreturning platform shutdown. Preparing peers does not mark them
online or authorize S release. Public inspection still lacks a System-ledger
and named-thread/hart enumeration query; pool/domain labels are not a substitute.

Before releasing a secondary, the core still needs secondary System-context
admission, compatible per-CPU ISA/vector frame
validation, primary-owned physical-device maintenance and qualified cross-hart
scheduling/reclamation. Returning monitor preservation and terminal takeover
must also be qualified with actual S-mode peers. Mapping mutation, ASID reuse and frame reclamation
also require their remote TLB/lifetime contract. The current immutable boot roots
do not imply that such a dynamic protocol exists. No SBI service is assumed.
Multi-Scheduler may later supply placement and activation policy; it cannot
bypass these core safety gates.

## State audit

| Current state | Present assumption and required boundary |
| --- | --- |
| `kernel.rs` guarded managers and immutable images | One-shot startup and bounded guards remain. Shared IMAG sources/aliases distinguish application and module roles; execution instances retain actual source/code pins. Checked RV64 RLOC metadata permits independent copies with separate code/data/BSS spans. Module arena backing remains reserved until reset. General dynamic linking, hot replacement and physical code-page unloading remain unsupported. |
| `ThreadContext::traps` | The RV64 scheduler selects the running thread context; nested accounting belongs to that OS thread. A separate bootstrap context is used for synchronous firmware service execution. |
| `Desktop` | The global compositor, physical input and window registry remain shared. Trusted app context selects saved events, menus, dialogs, cursors, worlds, font/legacy state and TextEdit managers at the masked service boundary. Windows carry an owner; background retained scenes are composed centrally. |
| Application resources | Read-only application-to-System/font search selects the caller's immutable image. Retained content replay selects the owning image, so a background window does not resolve against whichever application invoked the compositor. General mutable Resource Manager chains remain open. |
| Scrap Manager | One shared guarded System clipboard. Native edits stage fallible changes before publication. Original private scraps and process suspend/resume transfer remain unimplemented. |
| Memory Manager | One synchronized resident store owns registered MMBL blocks and MMST per-domain arena/projection/MemErr state. The scheduler binds the authentic OwnerRef; traps never select memory by scheduler slot. Shared boot-wide projection generations reject stale/foreign 32-bit IDs, including after zone destruction. The generic walk owns block and backing release. |
| File Manager | One mounted catalog/device service with owner-tagged fork marks. The native trap captures AppId before I/O handoff; handle read/seek/tell/close checks that identity. Exit closes only its owner's marks. Internal loader requests use reserved owner zero. |
| Framebuffer, input and allocator | One-hart interrupt-masked kernel work serializes device and heap access. Application and module-worker bodies are preemptible. This is not a multi-hart synchronization or blocking-mutex design. Secondary harts remain parked. |
| Firmware `BootVolume` and disk callbacks | The original boot call remains live. `machine/io.rs` masks interrupts and selects the permanent bootstrap executor on the shared per-hart System stack, carrying the original resource/allocation/I/O selections across the handoff. It is not an independently runnable I/O thread or an asynchronous queue, and the opaque firmware context is not given a Send implementation. |
| Host test-support | Host TLS, allocation/synchronization and substituted gates do not establish native RV64 trap, preemption, allocator or cleanup behavior. They provide no OS acceptance evidence. |

## Legacy globals and native contexts

The 68k adapter must virtualize application globals through the trusted AppId;
it must not write one low-memory image when switching apps and allow another
thread to observe it concurrently. Native Rust callers use typed context-owned
services directly. Legacy code should retain its original observable behavior
without requiring source changes, within the captured compatibility profile.

| Original implicit state | Native ownership/mapping |
| --- | --- |
| Current application resource file (`CurApRefNum`), resource search chain and `ResErr` | AppState resource context; shared immutable System catalog is the fallback, never another app's resource file |
| `thePort`, font/text/pen/clip selection and app window/control/menu lists | AppState object identities and port state, checked against AppId; only documented Thread Manager save/restore fields become LegacyContext-specific |
| `MemErr`, `TheZone`/`ApplZone`, A5 application world and partition limits | AppState Memory partition and compatibility error/zone views; ThreadContext owns registers/stack and LegacyContext owns its saved A5/register image |
| Default volume/directory and Standard File preferences | AppState current-directory/default-volume context; persistent settings go through the JSON preference service when writes exist |
| Application event mask/queue, current process information and menu selection | AppState event/process/menu services; hardware input is routed by the UI/device owner, not consumed from one unpartitioned app queue |
| TextEdit scrap references and cached per-app clipboard data | App-owned handles referencing the shared system Scrap service; clipboard ownership and format transfer are synchronized |
| Tick/time counters, physical screen descriptors, hardware driver tables and immutable System resources | Actual system services/state with documented access and synchronization; these are not duplicated per app |
| Cursor/foreground/menu-bar presentation | System UI owner commits the active app's requested state; background app requests do not directly mutate another app's display |

An `AppState` is not automatically one Rust TLS value: additional native threads
of the same application share its application-global fields under synchronization.
Thread-local error behavior, current ports or context switch saves must follow
explicit native contracts or original observations, not a blanket assumption that
all historical globals were thread-local. Raw original pointers need checked
address-space/object translation rather than access to resident Rust addresses.

## Synchronization rules

1. Use safe ownership and established no-std synchronization primitives. A mutable
   manager guard must not expose aliases or allow references to escape its guard.
   `Rc`/`RefCell` are not production shared-state synchronization.
2. Do not busy-wait for an app, service or device lock held by another scheduled
   thread. Contention must return a defined temporary/busy result or block the
   caller through the OS scheduler and wake it when eligible. A `try_lock` can
   provide a bounded bootstrap boundary; callers must not turn it into a spin
   retry loop in the resident kernel.
3. Do not hold a global manager lock across application-defined procedures,
   definition resources or nested traps. Capture owned work/state, release the
   guard, invoke the callback, then reacquire and validate owner/generation before
   committing. Keep already supported definition-to-Surface/font/resource nesting
   functional during this transition. Rejecting same-manager reentrancy is a
   bounded initial limitation, not a complete callback implementation.
4. Distinguish reentrancy from concurrency. A nested call in the same thread is
   not a different app; two independent threads are not excessive trap nesting.
   Errors and lock release must remain correct through callback failure, panic in
   host tests, cancellation and thread exit.
5. Avoid blocking I/O and allocation while holding unrelated manager guards.
   Prefer independent AppState and manager locks to a giant System lock. Define
   any necessary lock order and priority-inheritance policy before blocking
   mutexes appear; callbacks always run outside that order.
6. Shutdown first prevents new requests, cancels or drains in-flight work and
   releases owned resources; only then can app code, stacks and callbacks unload.
   Memory safety cannot depend on a process never exiting.

## Delivery gates

| Gate | Current implementation and required evidence |
| --- | --- |
| Boundary hardening | One-shot publication, bounded guards, owner-tagged file marks and trusted context routing exist. Host-linked fixtures do not establish native exclusion/cleanup contracts; scenario-specific target-runtime evidence is required. |
| Scheduler foundation | One-hart supervisor timer preemption, growable registered thread records, independently owned stacks/register frames, trusted identities and deferred retired-stack reclamation exist. A private RV64 observer progresses during a child's no-trap/no-yield loop on one hart; the canonical process acceptance passes. Native event wait deadlines, readiness wakes and a separate idle stack now exist; the SDK latency fixture verifies physical input during contention and that blocked apps leave the runnable rotation. General application blocking/thread APIs remain open; privileged module workers have the narrower implementation and native evidence described above. |
| Application launch | Finder and disk-loaded native APPLs have independent application threads, app-local UI/resource/memory state and exit cleanup. Host SDK ownership tests cover file denial/cleanup and memory generation retention; the real RV64 launch/quit/relaunch, partition/ownership, Pascal and private synthetic-event gates pass. |
| Application switching | Resident right-hand menu and Process Manager activation/hide/restore paths exist. Private SDK and actual Finder/two-app activation/hide/input/exit tests pass, including first-click activation and background dragging. The original popup comparison passes with explicit icon/cursor masks; this establishes the captured profile, not all application behavior. |
| Interactive latency | Window/file drag and selection feedback use bounded frontbuffer-only XOR without scene replays or a tracking-mask scan. Canonical captures retain every decoded move, including the first; strict all-samples sub-millisecond qualification remains open on fresh Windows and Linux guests. Shared instruction paths and perimeter-intersection early-outs reduce work but do not establish a hard latency guarantee. Interrupt-masked resident calls and cold execution still need their own bounds. |
| Reentrant service work | Existing definition-to-Surface/font/resource nesting remains supported. Stateful recursive manager calls still return Busy. General app callbacks require snapshot/callback/commit and cancellation tests. |
| Legacy Thread Manager | Planned as [compatibility workstream P2](docs/CORE-TOOLBOX-ROADMAP.md#p2--legacy-thread-manager-cooperative-threads); not implemented. Capture the pinned extension's traps, scheduling, state, stack/global context and callbacks, then verify through SDK and translated 68k probes. Cooperative contexts remain inside their host application's OS thread and cannot replace kernel preemption, including during legacy critical sections. |
| Multi-hart and devices | Planned as [platform workstream P1](docs/CORE-TOOLBOX-ROADMAP.md#p1--multi-hart-execution-smp); not implemented. Per-hart entry/context state, concurrent scheduling and inter-hart wakeups, migration, blocking/wakeup ordering, allocator/device synchronization and concurrent callback publication need independent evidence. |

Canonical tests belong under `crates/mac-toolbox/tests` and use the public SDK.
Host-linked test outcomes are not OS acceptance. Real RV64 scheduler, interrupt,
allocator and application paths require their own scenario-specific evidence. Distinguish native
threading extensions from classic 68k ABI behavior and never report a spinlock,
atomic counter or successful single-app boot as a complete multithreaded OS.

Apple's original [Introduction to Processes and Tasks](https://developer.apple.com/library/archive/documentation/mac/pdf/Processes/Intro_to_Procs_Tasks.pdf)
describes the historical cooperative application model. The requirement here
intentionally changes the native scheduler contract while retaining captured
application-visible manager behavior. [Apple's Thread Manager manual](https://leopard-adc.pepas.com/documentation/macos8/pdf/ThreadManager.pdf)
describes cooperative yield operations within applications; availability and
variant behavior still require the pinned 7.6.1 reference.

## Bounded implementation seam

`state::TryState<T: Send>` uses the established `spinning_top` mutex exclusively
through `try_lock`. Memory and desktop callers receive
`AccessError::{Unavailable, Busy, WrongOwner}`; these map respectively to
`NOT_SUPPORTED`, `SERVICE_BUSY` and `WRONG_THREAD`. Busy is a temporary ownership
condition, not a missing manager. No kernel retry loop spins behind a preempted
owner. `OwnerState` keeps the non-Send firmware storage context in its fixed-owner
synchronous I/O service context; its unsafe construction is confined to the
bootstrap boundary. This context uses the shared per-hart System stack and is not an independently
runnable service thread.
`spin::Once` publishes immutable font/resource catalogs and logging callbacks.
A one-shot entry guard prevents live manager replacement and races in heap setup.

`context.rs` carries AppId and ThreadId. Active app-local state is saved by its
resident managers, including the classic font selection. An earlier unused
`LegacyGlobals` record and always-empty cooperative-context field were removed:
copying placeholders was not implementation of low-memory globals or original
68k Thread Manager save/restore. Those remain required legacy work; add each
state field with its actual service and ownership contract. Only system-wide catalogs, device
ownership, compositor state and system configuration stay shared. A global
current-app variable is prohibited.

Trap accounting is stored in `ThreadContext::traps`, with an RAII scope restoring
its nesting count on exit. The current accessor selects the scheduled RV64
thread and uses a separate bootstrap service context during synchronous firmware handoff. The one-hart
provider and interrupt discipline must become per-hart before migration or
parallel execution is enabled.

`concurrent_services.rs` exercises one shared production holder/Memory Manager
through public SDK calls from eight host OS threads. It verifies request
atomicity, distinct-handle contents, progress after contention and explicit
cleanup/reuse. `Handle::try_dispose(&mut self)` preserves ownership on Busy so
callers can retry without losing the handle. Existing Drop remains best-effort;
scheduler-aware blocking disposal or owner-queued deferred cleanup is required
before general concurrent RAII cleanup can be advertised. Host-only yields in
this test are not a proposed resident scheduler or kernel retry policy.

The SDK also provides an owned `Window` through `Windows::create_owned`.
`close(&mut self)` retains ownership on failure; Drop makes a non-panicking
close attempt with the same current contention limit. Its copied `WindowId`
only identifies events/drawing, and `into_id` explicitly transfers cleanup.
Graphical userspace ownership acceptance covers scope exit, early returns with
both window and memory owners, externally closed windows and ownership transfer.

## Rust standard library readiness

The long-term application model includes Rust `std`, and Finder should use it
once the OS platform layer exists. The resident kernel, ABI and transport remain
`no_std`. Build the Rust `std` platform abstraction layer on a minimal
`core`/`alloc` ABI and transport substrate, not through a dependency cycle in
which building `std` requires the application-level `mac-toolbox` facade. The
facade remains `no_std` and usable by applications with or without `std`.

The detailed [P8 module map and bring-up plan](docs/RUST-STD.md) covers the target/
sysroot patch, runtime, per-service gaps and S0–S5 acceptance. That inventory is
the std delivery plan; the table below summarizes its threading/service boundary.

| Platform layer requirement | Required delivery gate |
| --- | --- |
| Stable aligned pointer allocation, deallocation/reallocation and failure policy | Define raw allocator ABI, alignment/zero-size rules, ownership and preemption-safe synchronization. The current movable/opaque Memory Manager handles are not a GlobalAlloc backend. |
| Application entry and termination | OS starts each application's main thread with owned arguments/environment/streams; exit drains threads and manager work before unloading. Independent application entry/exit exists; owned arguments, environment, streams and worker-thread draining remain open. |
| OS thread spawn/join, TLS and destructors | Real kernel scheduling/stacks, per-thread platform data and deterministic cleanup. `std::thread_local!` and Toolbox execution identity must use the same actual ThreadId. |
| Blocking mutex/condvar or futex/park/wake | Scheduler queues, atomic unlock-and-wait, wakeup ordering, timeout and cancellation. No spinlock held across preemption or callback substitutes for these services. |
| Files and process-owned streams | Native UTF-8 paths, buffers, 64-bit offsets, open/read/write/flush/seek/error contracts, stdin/stdout/stderr and working directory. Legacy Mac Roman/HFS/fork conventions remain explicit adapters. Bounded whole-data-file creation/replacement now exists; general writable streams, descriptor semantics and pipes remain open. |
| Networking / `std::net` | Planned [networking workstream P7](docs/NETWORKING.md): resident TCP/UDP sockets, addressing/DNS, owned descriptors, partial I/O/EOF, readiness, park/wake/deadlines/cancellation and process-exit cleanup. The std adapter uses the minimal platform ABI; it does not link a separate stack or depend on the app-level Toolbox facade. |
| Clocks | Monotonic Instant independent of wall-clock adjustment, 64-bit SystemTime including 2026 and beyond, epoch/range/error tests. Original classic date counters are compatibility views, not the native clock's range limit. |
| Panic/runtime boundaries | First std port uses panic_abort. Never unwind through an ECALL frame, C ABI entry, WDEF/CDEF procedure or firmware callback. Unwinding support requires a separate ABI and cleanup audit. |
| Capability and errors | Truthful absent-device/unsupported results, distinct contention/ownership errors, thread-safe error mapping and versioned requests. Native std features must not silently change classic adapter semantics. |
| Preferences | Versioned typed JSON for system and per-app scopes selected by trusted context. Serialize concurrent writers and commit through atomic replace/transaction; legacy preferences/PRAM APIs adapt to this store. Bounded file creation is a foundation; typed settings/schema/migration/notification and transactional publication still require P6 services. Preferences are not a prerequisite for compiling std itself. |

Sequence the first allocating std app after target/startup, application-thread
and allocator gates; add general blocking, workers and TLS before advertising
concurrent std services, then broader files/process/network support. Keep a
native std test application separate from original 68k
compatibility tests. Compilation alone cannot establish spawn/join, TLS teardown,
blocking progress, time range or persistence. Finder's eventual std adoption must
continue using Toolbox services through the public facade rather than linking
resident implementation code.


## Architecture portability

The scheduler's AppId/ThreadId, future legacy-context identity, manager ownership, queues,
blocking/wakeup semantics and std platform services are architecture-neutral.
RISC-V is the first live machine. x86 is a future target, not a supported build;
whether that means x86-64 or 32-bit x86 is an explicit later platform decision.
Do not encode hardware hart/CPU IDs into application identity or add RISC-V CSRs
to Toolbox/context code. The current RV64 machine adapter supplies scheduled
thread identity; a second target must provide equivalent identity and trap-entry
contracts.

Keep trap entry/exit and register saves, thread stack/context switching, privilege
levels/MMU/page tables, interrupts/timers/IPIs, atomics/barriers and machine-device
backends in target adapters. Native resource code generation and ELF/linker/
reservation rules also belong to each target's toolchain profile. An RV64 code
resource is not executable x86 code; target descriptors and loaders must reject
mismatches explicitly. Pure Rust manager algorithms and public service semantics
remain shared.

The std platform layer targets architecture-neutral kernel services rather than
RISC-V ECALL instructions directly; only its transport/syscall adapter changes.
Version all wire layouts, state pointer-width assumptions and use fixed-width
protocol fields for persisted data. Explicitly translate pointers/sizes at each
native ABI boundary instead of assuming that `usize`, classic 32-bit pointers,
RV64 pointers and a future x86 ABI have the same representation. Cross-target
layout/codegen/boot/trap/context-switch tests are required before advertising a
second architecture; a Rust compile target alone is insufficient.

The shared handle facility allocates every treaty namespace's generations
across tables, including open forks, staged writes and volumes. Two live
manager instances cannot mint the same projected ID; dropping a
context does not reset history. Counter allocation is atomic and exhausted slots
retire. Memory keeps its high16/low16 wire, with payload and zone in the registered
owner store. Windows, controls, menus, TextEdit editors and offscreen worlds use
full-width opaque tokens resolving to authenticated ledger keys. TEDT/TESV own
editor payloads and backing in the actual per-app context, including cleanup from
saved contexts; no fixed editor count or private generation array remains.
ALRT/ALST retain classic Alert protocol and backing in that same authenticated
app context. The protocol, its window and its managed controls have independent
registered lifetimes: cleanup works without imposing parent/child kind order,
and a saved app's cleanup never foregrounds it. Taking the protocol invalidates
it before child cleanup; refused window cleanup retains its own ticket.
Native MenuId names a semantic menu property; MenuHandle identifies a live object. Owned menu references validate the latter on every call.
DRAG/DRST and DRCV/DRVS register drag sessions, receiver registrations and their
per-owner backing. Native and classic DragRef wires share the same checked
projection onto those keys. Registered release wakes surviving peers and removes
only outline/highlight feedback; it needs no ordered process-exit cleanup hook.
A leased tracking event can expire during a target probe. The response trap
reports Applied or Expired atomically with validation; Finder retains its last
applied target and document pins until a replacement is applied.

Application EXIT stops admission and retires its thread before registered cleanup. Saved
manager state stays owned by the process until another stack has reclaimed that
thread's stack/context and the authoritative ledger permits retirement. Actual
stack release schedules a bounded retry/reap checkpoint; ordinary input and
unchanged timer ticks do not scan or retry dying domains. A persistent refusal
retains the process, its saved managers and image identity, and prevents writable
image reload for that file. Cleanup resolves resources from that retained image
even after the subject has no scheduler thread; an unknown owner is refused
instead of resolving Finder's resources. Module worker stop follows the distinct cooperative contract above. Broader
file/provider teardown, failure-path coverage and paging remain separate gates.

The current native ThreadContext receives the directory's actual OwnerRef when
its thread is spawned; its scalar AppId is derived from that authority. Traps can
borrow that binding without taking the Desktop lock, including nested definition
service calls. The reference is released only when a different stack reclaims the
retired thread. Bootstrap/headless storage calls use the one System owner. Module workers bind the canonical module owner plus their independent original
allocation/I/O roots and pinned resource image as described above. This does not
establish multi-hart admission or a privilege-isolation gate.

## Deferred driver I/O integration

The resident job pump now advances at most one bounded callback per scheduler
sample, with an atomic early-out when no jobs are ready. It takes owned work out
of the queue, releases the queue guard, invokes the provider through the checked
module broker, and reacquires the queue only to commit its completion. Provider
admission contention is distinct from a terminal provider error; unsent work
retains its original owned input and can still be cancelled without dispatch.

Outer native traps can park on a generation-tagged job after all request borrows,
legacy string conversion temporaries and manager guards have gone out of scope.
The saved PC/registers are replayed after completion so the adapter gets fresh
borrows. The adapter must retain its owned operation state, consume the result
once and return `SERVICE_BUSY` when requesting a park. The boundary rejects an
attempt to replay an already successful call. Nested definition/drawing traps
cannot park. Completion-before-park is checked under the scheduler guard; the
completion path releases the job guard before waking the scheduler.

Scheduled thread identities now use the shared `Threads` object namespace rather
than deriving IDs from AppId. Late completions validate the thread's generation
and its exact waiting job. Event readiness cannot wake an I/O wait. Retiring a
thread discards delivery and starts cancellation drainage; buffers and broker
requests stay owned until acknowledged, independent of the reclaimed app stack.
An `OwnedJob` provides the same abandonment behavior on a resident adapter's Drop.

Resident-owned work may attach a separate waiting OS thread. A waiter has no
authority to consume/cancel that result, cannot replace another thread's
attachment, and is checked by exact thread/job generations at park and wake.
Thread exit detaches that waiter without discarding a kernel-owned completion.
This lets File Manager retain publication/cleanup state after an app exits.
`OwnedJob::cancel` requests cancellation while preserving actual acknowledged
results; dropping the owning job still abandons delivery and drains cancellation.
Completion-before-attachment and completion-before-park both remain observable
as ready, and cancellation completed before dispatch wakes an attached waiter.

Provider-backed data saves now retain their RAM buffers, exact parent/expected
file identity and generation-tagged write marks in resident state. Parent/lookup,
begin, batched write and commit/abort callbacks run after Files guards and native
firmware handoffs are released. The waiting application owns no driver-facing
buffer. Actual publication updates volume/catalog state and retires the mark in
one Files operation; cancellation cannot claim an acknowledged write was undone.
App exit removes RAM-only staging and keeps active or uncertain provider leases
until their cleanup acknowledgement. A failed cleanup keeps the mark quarantined
instead of allowing slot reuse. The native SDK gate covers parked saves and a
second app exiting with forgotten read marks/staged writer; exit during pending
I/O, disconnect and interactive latency still need their own gates.

This supplies the integration boundary for filesystem drivers; it does not by
itself establish live host filesystem behavior. Public mount/read/write tests must
exercise parking, cancellation/disconnect, other-app progress and single delivery
of mutations before the host mount milestone is complete. SMP, general application worker threads and unresponsive-provider timeout
recovery remain separate work. Privileged module workers can use the checked
broker with their original durable I/O Subject.

## Native async/await runtimes

The later Async I/O extension supplies awaitable I/O APIs. Resident mechanisms
provide safe owned requests, completions, cancellation and wakeups; extension
policy and runtime adapters use ordinary versioned registrations. This does not
move a Tokio dependency or a fixed async-provider list into the kernel.

Scheduling policy is also replaceable. The loadable **Multi-Scheduler Extension**
takes over from the resident single-hart boot scheduler and decides when to
activate additional harts. Core owns the architecture mechanisms for hart
startup/parking, interrupts/IPIs, context switches and safe synchronization.
Takeover is checked and atomic: rejection leaves boot scheduling usable, and no
additional hart is released before per-hart context, memory and interrupt/lock
safety gates pass. This remains the model's isolated scheduler capstone, not an
SMP claim for the current kernel.

Application identities use the shared facility's full u64 process-domain
incarnation, separate from scheduled thread IDs. The owner directory retains
each process's authoritative ledger; retiring a process requires an empty ledger
and its incarnation can never be reissued during that boot. Directory storage
reuse cannot accept an old ProcessId or another manager's ID. Finder's boot
identity is issued by that same facility and published explicitly;
desktop access, drag registration, crash policy and lifecycle notifications do
not assume that Finder is application 1. Native and Pascal conversion traps use
these same resident lookups; this does not add a 68k Process Manager adapter.
The application menu retains full-width identities in its resident actions;
MDEF's u32 command is a row selector, not a truncated process ID. Window-owned
resource resolution also retains the full process identity. The directory has
no fixed-eight limit; scheduler stacks/contexts and other remaining fixed-eight
storage still await their staged migration, so this alone does not admit more
than eight running apps.

Opt-in event tracing uses private snapshot format 2. Its six-word header is
followed by 16 full application identities, then the existing service counters,
event counters and ring records. Counter rows are assigned to observed identities,
never indexed by a truncated/clamped ProcessId. A full identity map stops admitting
additional counter rows; exact event identities remain in the bounded ring.
Capture tools decode only the current format and discover launched IDs from
complete resident registration/launch records.

The [base SDK async contract](docs/ASYNC-SDK.md) supports a std-compatible,
event-driven executor and optional Tokio over the same resident completion
notifications. Futures never retain manager guards across await. UI task polling
stays on the owning OS thread; Send/Sync object mobility does not waive
WrongThread checks. Privileged module workers can use the checked broker under their durable I/O
Subject; general application-thread APIs remain open.
Cancellation must distinguish confirmed cleanup from a racing committed mutation.


Broker request state is now a registered scoped `DREQ` resource, with `DRQS`
backing, rather than a private request table. The synchronous thread frame carries
the admitted I/O subject across nested module calls and the bootstrap stack
handoff. The runtime's exclusive registry borrow protects request metadata;
no borrow crosses a callback. Continuations validate the retained publication
receipt and the same admitted scope independently of caller policy. The common
walk retains unique tickets through actual lower acknowledgement, and native
cleanup uses owned state admission so a busy registry cannot discard a ticket.
Serialized-provider occupancy changes on admission/terminal reply; polling does
not scan all requests. This establishes neither full four-part invocation
accounting nor arbitrary privileged-fault containment or multi-hart activation.

Each canonical DREQ owns its original Start bytes and admitted reply capacity.
The runtime validates authority and sizes before preparing that storage, then
rechecks publication. A unique delivery lease moves the buffers outside the
registry; no registry or mailbox guard spans foreign execution. If a published
request has not entered its provider, a correlated Poll delivers Start with the
original bytes. Cancel before entry acknowledges absence of effects without
calling the provider. Once entered, Poll and Cancel retain their normal meaning.

The lease commits a returned terminal status and bytes before trying to reacquire
the registry. Busy retries replay that result instead of repeating provider
effects. The provider always receives the original admitted reply capacity; a
smaller caller retry buffer can refuse the final copy without losing the result.
Malformed replies or entry without a returned result retain uncertainty for
quarantine; they do not fabricate completion or claim privileged-fault recovery.

Terminal disposal releases the actual cancellation buffers, dispatch buffers and
record outside the registry, before the canonical Removed guard acknowledges
their retirement. The wire ticket clears only after that disposal. Cleanup races
retain the same guard in the existing request slot. Preparation and invocation
leases wake pending retirement after their actual alias decrement; a cleanup
waiting on an active lease subscribes to that handback under the same short
mailbox gate. No per-input retry scan or second request-ID scheme is involved.
The current borrowed queued Broker interface still owns its reconciliation
buffers as well as this DREQ storage. That additional backing is charged by the
real allocator; its peak under ordinary heap exhaustion remains unqualified.


Native driver-job wake identities now carry the shared facility's complete u64
job ID. The typed defer key keeps them distinct from file-transfer IDs. Admission
reserves return/notice storage for all retained jobs before publication, including
callbacks already detached while later jobs enter. Owner discard uses ordered
completion receipts with no fixed-size cleanup scratch array. The queue retains
its existing IRQ/preemption gate and borrowed origin guard; foreign callbacks
hold neither queue nor registry guards. This removes a projection/capacity limit,
not the remaining physical DMA/source, page, accounting or isolation gates.


Device/DMA policy now uses System-domain physical targets and native module
aliases/maps from the same registered facility. The short exclusive device-state
borrow covers claim counts and bus reset/probe bookkeeping; it never waits for
hardware. Cleanup retries probe once, keeping unique tickets quarantined until
a positive stop and actual mapping retirement. Ordinary invalidated IDs cannot
be revived by reset continuations. Losing the manager quarantines its live uses
without acknowledging DMA completion. This is a lifetime/ACK policy, not a proof
of physical page pinning, reclaimable module images, IRQ-driven reset progress
or multi-hart device synchronization.
