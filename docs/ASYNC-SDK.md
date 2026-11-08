# Native event dispatch and asynchronous execution

Native applications may opt into two userspace libraries:

- `sr-event-dispatch` owns weak subscriptions, target routing and serialized
  handler queues. It requires an application-selected local runtime.
- `sr-event-async` supplies the distribution's single-threaded event runtime,
  native readiness futures, cancellation scopes and the runtime integration
  interface. An application explicitly constructs the engine; neither std nor
  the Toolbox facade starts one automatically.

Applications remain free to use Toolbox events directly. The resident System
never executes a Rust future, Waker or application callback. The public Toolbox
facade remains `mac-toolbox`; platform wait operations remain usable independently
of either library. This replaces the earlier mandatory base-SDK executor policy.

## Ownership and event delivery

A parent owns `Rc<RefCell<T>>`. A `Handler<E>` stores a weak reference to that
object and a callback factory taking `&mut T` and an owned event. It temporarily
upgrades the reference for the synchronous callback factory. The factory returns
an owned local future; it cannot retain that mutable borrow across suspension.
The struct need not contain a permanent reference to itself.

Each Handler has one ordered queue and at most one active operation. Copies of
that Handler share the same queue across all subscribed sources. While an
operation awaits, subsequent events for that Handler remain queued. Other
Handlers can progress. Explicitly constructing another Handler creates another
queue. The engine gives other runnable work a turn between queued deliveries.
This serialization includes keyboard, paint and resize events: apps should avoid
long operations in a handler when they want that same handler to remain responsive.

`Router<K, E>` maps System-delivered target identities to Handlers. Routing
belongs in the library; applications need not visit every window on each input.
`Source<E>` is a direct object-local subscription, suitable for a control wrapper.
It sends to its Handler without visiting the app's Router. Neither path owns the
recipient. Router pruning and Source pruning remove dead weak recipients even
without a new event for that recipient. Owners call these during idle maintenance.

A control tracking callback is synchronous today. Scheduling an asynchronous
handler does not change the resident tracking protocol or make a synchronous
callback awaitable. Applications must keep the existing immediate feedback path
until a control wrapper supplies its own separately borrowable view state.

Queues use fallible growth rather than an arbitrary event count limit. Admission
failure is returned to the producer; it must report or recover from the failure.
No library silently discards an accepted event to make room for another one.

## Runtime boundary and cancellation

The `Runtime` interface accepts an owned local future and a cancellation scope.
The supplied engine's `Spawner` is a weak scheduling handle: pending operations
cannot create a cycle that keeps their engine alive. A Waker contains only a
thread notification right and an atomic ready flag. Its thread-safe wake
bookkeeping does not make the engine, handler, or UI objects Send or Sync.

The caller owns a `ScopeOwner`; tasks receive only its cancellation state. Child
scopes inherit cancellation. After a window close is approved, cancel its scope
and remove its subscriptions. Task disposal happens after the current poll
returns, never in the middle of application code. Explicit app-owned cleanup
can continue draining pipes and collecting real child results after the window
has closed. Cancelling a readiness future releases its subscription; it does not
claim to undo bytes already written or to terminate an unrelated child process.

The engine never holds its task-store borrow while polling user code. Readiness
subscriptions are weak and disappear when their futures are dropped. A wake
causes only ready tasks to be polled; pending operations are not polled because
an unrelated input event arrived.

An adapter for another runtime must implement the same local scheduling,
ordering, cancellation and wake integration contract. In particular, Tokio may
keep GUI tasks on its local task set and use thread-safe messages for background
work. The source interface permits such an adapter; a System RISC Tokio port and
its I/O/timer-driver integration are not supplied or qualified by these crates.
Two independent indefinitely blocking event loops on the same thread are invalid.

## Resident readiness and parking

The native readiness service waits on existing owner-checked pipe endpoints,
retained child-result identities, owner-scoped Time Manager tasks, absolute
monotonic deadlines and a durable
same-process thread notification. It also consumes ordinary native UI events
with their resident-captured pointer coordinates. It does not introduce a classic
EventRecord kind or route native readiness through the lossy UI event queue.

Readiness is level-triggered and non-consuming. A readable source reports bytes
or real EOF; a writable source reports enough space for the requested atomic
write, or peer close. An invalidated endpoint is reported as invalid. The caller
still performs its I/O through the original endpoint. Observing readiness grants
no new authority and does not promise that another consumer cannot change the
condition before use. Futures never lend user I/O buffers across Pending.

The service checks sources without holding SCHEDULER alongside their managers.
Source producers advance a revision and wake matching sleepers. Parking checks
that revision under SCHEDULER, closing the check-before-park race. A revision
change during checking returns control for a fresh check. Notifications are
sticky until collected, including a notification preceding the first wait.
Existing scheduler event wakeups still deliver UI input normally.

Source snapshots own only copied identities. No user address is retained across
a wait. Source storage is reused on subsequent waits and detached before an
owner resource walk. Stop, endpoint transfer and normal close invalidate the
original authority through the existing owner/generation machinery.

An absolute deadline is converted to hardware ticks with upward rounding, not
recomputed relative to the time spent checking other sources. The scheduler
wakes a parked thread at expiry; actual delivery latency includes scheduling and
runnable work. No hard real-time precision or new hardware qualification is
claimed. Cursor deadlines never serve as an excuse to issue read/write probes.

## Timers and Macintosh compatibility

`mac_toolbox::events::TimeTask` owns a generation-tagged resident task admitted
through the central object ledger and the caller's memory budget. Native
event-loop timers use the same OS monotonic deadline facility. It supports install, prime, fire acknowledgement,
remove and extended drift-free scheduling. Positive PrimeTime-style delays mean
milliseconds; negative delays mean microseconds. A task remains installed after
firing and can then be primed again. An active task must be removed and
reinstalled before rescheduling. Extended timers retain their scheduled anchor
rather than accumulating callback lateness. Removal invalidates the canonical task identity, wakes outstanding observers
and reports remaining time; extended reinstall with zero delay can resume the saved deadline.

`prime` returns the actual scheduled monotonic deadline. Readiness is
non-consuming; `fire` acknowledges one due expiration before the task can be
primed again. Dropping an installed TimeTask attempts removal; its original
owner ledger remains responsible for cleanup if removal is refused.

The native `reset` operation retimes an installed timer in place and wakes
observers to recompute their deadline. Cursor resets therefore do not allocate
a new owner-ledger object for each keystroke. Classic PrimeTime semantics stay
strict; `reset` is an explicit native event-loop operation.

The lifecycle follows Apple's [Time Manager addenda](https://leopard-adc.pepas.com/technotes/tn/tn1063.html)
and [Time Manager reference](https://leopard-adc.pepas.com/documentation/Carbon/Reference/Time_Manager/Time_Manager.pdf).
Native delivery happens in the application's event runtime. This is not a claim
that the classic `InsTime`/`InsXTime`/`PrimeTime`/`RmvTime` A-line adapters, TMTask
memory updates or emulator interrupt-level callback execution are implemented.
Those require a legacy callback boundary; native timer events must never be
inserted into a classic application's event queue as a substitute.

## Terminal consumer

Terminal opts into both crates. A collection owns per-session weak handler
recipients and window scopes. The native wait includes only the live session's
stdout/stderr readers, stdin writer when bytes are queued, a retained child exit
receipt after output has drained, and a cursor timer for an active session.
Only the ready stream is read. Timers do not query child status or poll streams.
A queued readiness/timer notice disarms its source until the handler consumes
it, preventing a busy handler from repeatedly enqueueing the same level-ready
condition.

Launch still uses the existing worker thread because executable loading is a
synchronous, potentially parking service. Completion publishes its channel
result and wakes the UI with a durable thread notification; there is no periodic
launch-completion fallback. Moving loading itself to a genuinely nonblocking
operation is separate work, not a consequence of putting `async` on a function.

Approved window close cancels window-scoped handlers. EOF/drain/reap remains
explicit app-owned work until the real child result is collected. A shell exit
closes its session window. An explicitly launched command leaves its window open
with its exit code. Closing the last window keeps Terminal available for a new
session; explicit Quit waits for owned cleanup.

## Evidence boundaries

These contracts require actual optimized target builds and native execution.
Compilation alone does not qualify wake races, cancellation, owner cleanup,
multiple sessions, absence of polling, legacy compatibility or performance.
Native qualification must record the exact paired artifacts, SDK, source,
configured memory, execution policy, scenarios and remaining gaps. Host Rust
tests are not a substitute, and no replacement Rust test framework is provided.
