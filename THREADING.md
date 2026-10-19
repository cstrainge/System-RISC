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

This is a required architecture contract, not a description of an implemented
scheduler. Current firmware parks secondary harts, disables interrupts and starts
Finder by a direct function call. There is no timer-driven context switch,
thread-owned kernel stack, runnable queue or application-thread launch yet.
Existing one-application boots and host concurrency tests cannot establish those
features. They remain concrete delivery gates below.

## Execution identities

The scheduler must supply a trusted execution context at trap entry:

| Identity | Owns or selects |
| --- | --- |
| `AppId` | Resource chain/current resource file, Memory Manager partition and errors, open-file ownership, events, windows/controls, ports/fonts/text state, callbacks and cleanup |
| `ThreadId` | OS stack/register state, blocking/wakeup/cancellation, trap depth and callback stack, error state explicitly specified per thread |
| `LegacyContextId` | Optional cooperative Thread Manager register/stack/A5 state inside one application; never substitutes for ThreadId |
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
| `kernel.rs` guarded managers and immutable published slots | One-shot startup now publishes logging/font/resource data through Once and mutable managers through bounded guards; repeated entry is rejected instead of replacing live values. App ownership and future scheduler-aware blocking remain separate gates. |
| `ThreadContext::traps` | Trap dispatch now uses per-context RAII accounting rather than global DEPTH/NESTED. The current accessor deliberately supplies only the bootstrap thread; the scheduler must select each running thread's context before launching more threads, including migration. |
| `Desktop` | Currently combines global compositor/input with application ports, event queue, controls and retained scenes. Split device/UI owner services from AppState; UI requests carry trusted AppId and return events to that app. A UI service OS thread is legitimate; a single cooperative loop scheduling all apps is not. |
| Resource `Files.application`, current fonts, classic port/font state | Immutable System catalogs can be shared. Application resources/search state and graphics selection belong to the correct app/context. Published slices remain alive across asynchronous requests and exit until references drain. |
| Scrap Manager | One system clipboard is deliberately shared and guarded by TryState. Requests copy payloads; no borrowed pointer escapes. Eight host threads exercise this real shared holder. Per-app private scraps, ownership/suspend-resume and scheduler-aware retry remain future integration. |
| TextEdit | Generation-checked records belong to the guarded Desktop today. Native clipboard operations acquire Desktop then Scrap, stage fallible edits before publication, and return copied metadata. Future AppState must own editors and validate AppId; IDs alone are not isolation. Classic private TE scraps remain unimplemented. |
| Memory and File Manager | Mutable arenas and fork marks are currently shared singleton state. Context-local instances or owner-checked tables are needed. Firmware storage callbacks have opaque pointers and serial-call requirements; they are not implicitly Send/Sync or interrupt-safe. |
| Framebuffer/backbuffer and virtio input queues | Hardware accesses use mutable static storage with implicit single-owner access. Give compositor/device workers explicit ownership; never expose an unrestricted `'static mut` buffer to arbitrary app threads. Interrupt handlers enqueue bounded work rather than reentering managers. |
| `LockedHeap` in ROM/System | Its allocator spinlock is not a scheduler-aware mutex. A preempted lock holder can deadlock a same-core allocator caller. Before enabling preemption, provide a bounded nonpreemptible allocator critical section or a proven scheduler-aware allocator; prohibit allocating interrupt paths. |
| Firmware `BootVolume`, disk callbacks and ROM stack | Borrowed stack/context remains alive only while the original System call is live. Device ownership/lifetime must survive app threads, asynchronous I/O and shutdown. An I/O-owner OS thread can serialize the existing callbacks without asserting unrestricted thread affinity. |
| Host test-support TLS `RefCell` | Test-only isolation, not production synchronization and not proof that callers share one manager. Concurrent acceptance must enter one shared real service fixture. |

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

| Gate | Required evidence |
| --- | --- |
| Boundary hardening | One-shot startup publication, bounded synchronized manager access, explicit owner/context plan and no new unchecked shared mutable globals. Shared real-service aggregate SDK concurrency tests verify exclusion, progress and cleanup. This does not provide OS threads. |
| Scheduler foundation | Kernel-owned ThreadId, stacks/register saves, timer interrupt, runnable/blocked queues, wakeups and exit. Tests prove an independent OS thread runs when another performs no cooperative yield, including one-hart operation. |
| Application launch | Finder and each independently loaded app are launched as separate actual OS threads. Per-app memory/resources/events/ports and owner-checked IDs pass simultaneous public SDK calls. A shared-stack function call, host thread or cooperative coroutine does not satisfy this gate. |
| Application switching | The system-owned right-hand application menu reflects live Process Manager identities. Switching routes windows, app menus and keyboard events to the foreground AppId; hide/restore preserves state and quit removes stale entries. Background apps retain independent scheduled execution. Test Finder plus two native apps, including a nonyielding background thread. |
| Reentrant service work | Snapshot/callback/commit paths release manager locks and preserve generation/owner state; nested WDEF/CDEF/resource/font traps and app callbacks pass under preemption and cancellation. |
| Legacy Thread Manager | Original version-filtered APIs and errors pass inside one app's OS thread. A nonyielding context can stall sibling legacy contexts, but cannot prevent other apps or kernel threads from running. Context switches preserve the original app-visible state. |
| Multi-hart and devices | Per-hart entry/interrupt stacks, thread migration, allocator preemption safety, device ownership, wakeup ordering and concurrent callback publication are proven. Removing the secondary-hart park alone is insufficient. |

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
owner. `OwnerState` keeps the non-Send firmware storage context on its verified
I/O thread; its unsafe construction is confined to the bootstrap boundary.
`spin::Once` publishes immutable font/resource catalogs and logging callbacks.
A one-shot entry guard prevents live manager replacement and races in heap setup.

`context.rs` names AppId, ThreadId and LegacyContextId and provides an owned
`LegacyGlobals` shape: resource-file selection, graphics port/font, memory and
resource errors, volume/directory, event mask, A5 world, application zone and scrap
reference. This shape is a routing contract, not implemented low-memory
virtualization: existing managers still need to move their corresponding fields
into the actual selected AppState. Only actual system-wide catalogs, device
ownership, compositor state and system configuration stay shared. A global
current-app variable is prohibited.

Trap accounting is stored in `ThreadContext::traps`, with an RAII scope restoring
its nesting count on exit. The current accessor explicitly supplies only the
bootstrap context; scheduling must install the running thread's context before
any additional thread can enter. It is not a per-hart counter disguised as a
thread counter. The temporary I/O owner provider likewise must change from the
single bootstrap hart invariant to the scheduler's actual ThreadId first.

`concurrent_services.rs` exercises one shared production holder/Memory Manager
through public SDK calls from eight host OS threads. It verifies request
atomicity, distinct-handle contents, progress after contention and explicit
cleanup/reuse. `Handle::try_dispose(&mut self)` preserves ownership on Busy so
callers can retry without losing the handle. Existing Drop remains best-effort;
scheduler-aware blocking disposal or owner-queued deferred cleanup is required
before general concurrent RAII cleanup can be advertised. Host-only yields in
this test are not a proposed resident scheduler or kernel retry policy.

## Rust standard library readiness

The long-term application model includes Rust `std`, and Finder should use it
once the OS platform layer exists. The resident kernel, ABI and transport remain
`no_std`. Build the Rust `std` platform abstraction layer on a minimal
`core`/`alloc` ABI and transport substrate, not through a dependency cycle in
which building `std` requires the application-level `mac-toolbox` facade. The
facade remains `no_std` and usable by applications with or without `std`.

| Platform layer requirement | Required delivery gate |
| --- | --- |
| Stable aligned pointer allocation, deallocation/reallocation and failure policy | Define raw allocator ABI, alignment/zero-size rules, ownership and preemption-safe synchronization. The current movable/opaque Memory Manager handles are not a GlobalAlloc backend. |
| Application entry and termination | OS starts each application's main thread with owned arguments/environment/streams; exit drains threads and manager work before unloading. Current direct-call entry is bootstrap only. |
| OS thread spawn/join, TLS and destructors | Real kernel scheduling/stacks, per-thread platform data and deterministic cleanup. `std::thread_local!` and Toolbox execution identity must use the same actual ThreadId. |
| Blocking mutex/condvar or futex/park/wake | Scheduler queues, atomic unlock-and-wait, wakeup ordering, timeout and cancellation. No spinlock held across preemption or callback substitutes for these services. |
| Files and process-owned streams | Native UTF-8 paths, buffers, 64-bit offsets, open/read/write/flush/seek/error contracts, stdin/stdout/stderr and working directory. Legacy Mac Roman/HFS/fork conventions remain explicit adapters. Bounded catalog/Trash writes exist; general file writes and atomic replacement remain M7 work. |
| Clocks | Monotonic Instant independent of wall-clock adjustment, 64-bit SystemTime including 2026 and beyond, epoch/range/error tests. Original classic date counters are compatibility views, not the native clock's range limit. |
| Panic/runtime boundaries | First std port uses panic_abort. Never unwind through an ECALL frame, C ABI entry, WDEF/CDEF procedure or firmware callback. Unwinding support requires a separate ABI and cleanup audit. |
| Capability and errors | Truthful absent-device/unsupported results, distinct contention/ownership errors, thread-safe error mapping and versioned requests. Native std features must not silently change classic adapter semantics. |
| Preferences | Versioned typed JSON for system and per-app scopes selected by trusted context. Serialize concurrent writers and commit through atomic replace/transaction; legacy preferences/PRAM APIs adapt to this store. Actual persistence depends on M7 writable-filesystem transactions and is not implemented by the current catalog/Trash-only write API. |

Sequence the first port after scheduler/application-thread, allocator and blocking
primitive gates; then add basic std I/O/time/TLS, followed by wider filesystem and
process support. Keep a native std test application separate from original 68k
compatibility tests. Compilation alone cannot establish spawn/join, TLS teardown,
blocking progress, time range or persistence. Finder's eventual std adoption must
continue using Toolbox services through the public facade rather than linking
resident implementation code.


## Architecture portability

The scheduler's AppId/ThreadId/LegacyContextId, manager ownership, queues,
blocking/wakeup semantics and std platform services are architecture-neutral.
RISC-V is the first live machine. x86 is a future target, not a supported build;
whether that means x86-64 or 32-bit x86 is an explicit later platform decision.
Do not encode hardware hart/CPU IDs into application identity or add RISC-V CSRs
to Toolbox/context code. The current bootstrap provider calls the machine
adapter; the scheduler must replace it with genuine thread identity.

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
