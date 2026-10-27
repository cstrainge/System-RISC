# Threading and ownership contract

Every application must execute on its own **OS-scheduled thread**. The kernel
scheduler owns runnable, blocked and terminated OS threads and supplies execution
time independently of calls to `WaitNextEvent`, `EventAvail`, Toolbox managers or
legacy yield functions. Finder is an application and follows this rule too.

Classic Thread Manager contexts are cooperative execution contexts **inside one
application**, hosted by its OS thread. `YieldToThread` selects only contexts of
that application; it cannot schedule another application or suppress kernel
preemption. Classic callback and A5/low-memory conventions are compatibility
state attached to the application/context, not global scheduler state. A later
native worker-thread API can expose additional OS threads explicitly.

The first native implementation now schedules Finder and separately loaded APPLs
on one RV64 hart. CLINT timer interrupts preempt application code independently
of Toolbox calls. Registered, fallibly grown application thread records own independent page-backed stacks (4 MiB by default, selected by the checked
platform policy within 64 KiB–8 MiB),
separately page-backed saved integer/FP/vector state (storage scales with hardware
VLEN) and
trusted AppId/ThreadId contexts. Kernel traps mask
interrupts, including nested definition calls, so allocator and manager guards
cannot be preempted by another application on this hart. Native event waits now
block the actual application thread, with a separate idle WFI context and 250 us
timer sampling. Readiness is inspected before taking the scheduler lock; a fresh
foreground input wake takes priority over background and timeout wakes. No manager
guard or borrowed request crosses the context switch. Worker thread APIs, general
blocking primitives, migration and multiple harts remain future work. The dynamic
capacity SDK gate now runs ten apps together, delivers physical keys to all nine
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

This is a trusted shared M-mode address space without an MMU or protection
boundary. Owner checks prevent accidental cross-application object access through
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
| Legacy cooperative context (planned) | Optional Thread Manager register/stack/A5 state inside one application; never substitutes for ThreadId |
| `HartId` | Hardware trap/interrupt stack and current ThreadId pointer; not the identity of an application |

Request payloads cannot impersonate these identities. The scheduler/service gate
supplies them. Native service-table capabilities are routing authority, not a
mutable global “current application.” Before multiple apps launch, every
application-owned object needs an owner and generation; IDs and cleanup must
reject cross-application access. If a classic API specifies application-global
rather than thread-local state, concurrent calls still synchronize that state;
its historical implicit global is selected through AppId.

## State audit

| Current state | Present assumption and required boundary |
| --- | --- |
| `kernel.rs` guarded managers and immutable images | One-shot startup and bounded guards remain. Eight immutable image-cache slots include Finder; app code/resources remain resident after exit. Checked RV64 RLOC metadata permits independent copies with separate physical code/data/BSS spans. General dynamic linking, hot replacement and unloading remain unsupported. |
| `ThreadContext::traps` | The RV64 scheduler selects the running thread context; nested accounting belongs to that OS thread. A separate bootstrap context is used for synchronous firmware service execution. |
| `Desktop` | The global compositor, physical input and window registry remain shared. Trusted app context selects saved events, menus, dialogs, cursors, worlds, font/legacy state and TextEdit managers at the masked service boundary. Windows carry an owner; background retained scenes are composed centrally. |
| Application resources | Read-only application-to-System/font search selects the caller's immutable image. Retained content replay selects the owning image, so a background window does not resolve against whichever application invoked the compositor. General mutable Resource Manager chains remain open. |
| Scrap Manager | One shared guarded System clipboard. Native edits stage fallible changes before publication. Original private scraps and process suspend/resume transfer remain unimplemented. |
| Memory Manager | One synchronized resident store owns registered MMBL blocks and MMST per-domain arena/projection/MemErr state. The scheduler binds the authentic OwnerRef; traps never select memory by scheduler slot. Shared boot-wide projection generations reject stale/foreign 32-bit IDs, including after zone destruction. The generic walk owns block and backing release. |
| File Manager | One mounted catalog/device service with owner-tagged fork marks. The native trap captures AppId before I/O handoff; handle read/seek/tell/close checks that identity. Exit closes only its owner's marks. Internal loader requests use reserved owner zero. |
| Framebuffer, input and allocator | One-hart interrupt-masked kernel work serializes device and heap access. App-only code is preemptible. This is not a multi-hart synchronization or blocking-mutex design. Secondary harts remain parked. |
| Firmware `BootVolume` and disk callbacks | The original boot call remains live. `machine/io.rs` masks interrupts and switches to a dedicated synchronous execution stack with fixed bootstrap ownership. It is not an independently runnable I/O thread or an asynchronous queue, and the opaque firmware context is not given a Send implementation. |
| Host test-support | Scoped TLS fixtures supply real resident gates. Shared service fixtures separately test guarded concurrency. Neither replaces actual RV64 timer, launch, switching and cleanup acceptance. |

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
| Boundary hardening | One-shot publication, bounded guards, owner-tagged file marks and trusted context routing exist. Shared real-service host tests establish the captured exclusion/cleanup contracts, not hardware scheduling. |
| Scheduler foundation | One-hart CLINT preemption, growable registered thread records, independently owned stacks/register frames, trusted identities and deferred retired-stack reclamation exist. A private RV64 observer progresses during a child's no-trap/no-yield loop on one hart; the canonical process acceptance passes. Native event wait deadlines, readiness wakes and a separate idle stack now exist; the SDK latency fixture verifies physical input during contention and that blocked apps leave the runnable rotation. General blocking primitives, worker APIs and cancellation remain open. |
| Application launch | Finder and disk-loaded native APPLs have independent application threads, app-local UI/resource/memory state and exit cleanup. Host SDK ownership tests cover file denial/cleanup and memory generation retention; the real RV64 launch/quit/relaunch, partition/ownership, Pascal and private synthetic-event gates pass. |
| Application switching | Resident right-hand menu and Process Manager activation/hide/restore paths exist. Private SDK and actual Finder/two-app activation/hide/input/exit tests pass, including first-click activation and background dragging. The original popup comparison passes with explicit icon/cursor masks; this establishes the captured profile, not all application behavior. |
| Interactive latency | Window/file drag and selection feedback use bounded frontbuffer-only XOR without scene replays or a tracking-mask scan. Canonical captures retain every decoded move, including the first; strict all-samples sub-millisecond qualification remains open on fresh Windows and Linux guests. Shared instruction paths and perimeter-intersection early-outs reduce work but do not establish a hard latency guarantee. Interrupt-masked resident calls and cold execution still need their own bounds. |
| Reentrant service work | Existing definition-to-Surface/font/resource nesting remains supported. Stateful recursive manager calls still return Busy. General app callbacks require snapshot/callback/commit and cancellation tests. |
| Legacy Thread Manager | Planned as [compatibility workstream P2](docs/CORE-TOOLBOX-ROADMAP.md#p2--legacy-thread-manager-cooperative-threads); not implemented. Capture the pinned extension's traps, scheduling, state, stack/global context and callbacks, then verify through SDK and translated 68k probes. Cooperative contexts remain inside their host application's OS thread and cannot replace kernel preemption, including during legacy critical sections. |
| Multi-hart and devices | Planned as [platform workstream P1](docs/CORE-TOOLBOX-ROADMAP.md#p1--multi-hart-execution-smp); not implemented. Per-hart entry/context state, concurrent scheduling and inter-hart wakeups, migration, blocking/wakeup ordering, allocator/device synchronization and concurrent callback publication need independent evidence. |

Canonical tests belong under `crates/mac-toolbox/tests` and use the public SDK.
Host tests are useful for shared-manager safety and race detection; real RV64
scheduler/interrupt/application tests are separately required. Distinguish native
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
bootstrap boundary. This context uses a dedicated stack but is not an independently
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

EXIT stops admission and retires its thread before registered cleanup. Saved
manager state stays owned by the process until another stack has reclaimed that
thread's stack/context and the authoritative ledger permits retirement. Actual
stack release schedules a bounded retry/reap checkpoint; ordinary input and
unchanged timer ticks do not scan or retry dying domains. A persistent refusal
retains the process, its saved managers and image identity, and prevents writable
image reload for that file. Cleanup resolves resources from that retained image
even after the subject has no scheduler thread; an unknown owner is refused
instead of resolving Finder's resources. The full module invocation context,
file/provider lifetime migration, controlled teardown retry and I/O death barrier
remain open; this checkpoint does not establish all of stage 3 or paging.

The current native ThreadContext receives the directory's actual OwnerRef when
its thread is spawned; its scalar AppId is derived from that authority. Traps can
borrow that binding without taking the Desktop lock, including nested definition
service calls. The reference is released only when a different stack reclaims the
retired thread. Bootstrap/headless storage calls use the one System owner. This
is an ownership bridge, not the later module executor/subject/allocation/resource
scope protocol, dynamic thread capacity, multi-hart admission or privilege gate.

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
of mutations before the host mount milestone is complete. SMP, general worker
threads and unresponsive-provider timeout recovery remain separate work.

## Native async/await runtimes

The later Async I/O extension supplies awaitable I/O APIs. Resident mechanisms
provide safe owned requests, completions, cancellation and wakeups; extension
policy and runtime adapters use ordinary versioned registrations. This does not
move a Tokio dependency or a fixed async-provider list into the kernel.

Scheduling policy is also replaceable. The loadable **Multi Hart Extension**
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
WrongThread checks. Worker-safe I/O can later use native worker threads.
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
