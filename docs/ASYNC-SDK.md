# Native asynchronous Toolbox SDK

Decision, 2026-10-21: native System RISC applications must be able to use
async/await easily. The base SDK owns the common machinery. The application
model targets an ordinary async entry point and awaitable services. Applications
must not build their own event/promise pump or executor. The first delivery slice
is lazy drag data; this decision does not claim that the full application runtime
or std target already exists.

Async I/O APIs are supplied by a later loadable System extension. The fixed
core owns the minimal request, completion, cancellation, lifetime and wakeup
mechanisms; the extension publishes its awaitable I/O services through ordinary
versioned, ledger-owned registrations. Do not hard-code an async provider or
executor into resident managers. The base SDK's event executor and optional
runtime adapters remain clients of those mechanisms. Removing an extension must
quiesce its registrations and retain outstanding requests until actual completion
or acknowledged cancellation; a timeout never substitutes for acknowledgement.

## One service contract, independent runtimes

Potentially waiting operations expose `core::future::Future` with typed results.
They use resident completion/readiness notifications and ordinary `Waker`s, not a
Tokio-specific task handle. Immediate queries remain cheap synchronous calls;
wrapping a blocking trap in `async fn` does not make it asynchronous. A future's
poll must do bounded work, register readiness without a lost-wake race, and return
Pending while other application work can progress. Completion wakes only the
interested work. Never poll pending I/O on every physical mouse event or on a
periodic busy loop.

The built-in executor is event driven and fully compatible with **std**. Its
normal application configuration supports allocation, owned futures, tasks and
ordinary Rust errors. A minimal no_std configuration may serve low-level clients;
it must not impose fixed-buffer/borrowed-only ergonomics on std applications.
The SDK event source, completion registry and OS-thread parking integrate with
that executor. A single receiver owns each app's queue; async tasks must not race
independent WaitNextEvent loops and consume each other's events. Windows retain
their ordinary decoded event streams alongside completion delivery.

An optional Tokio adapter will drive the same futures and event source. Tokio
must not be a dependency of resident managers or the minimal transport. System
RISC's std target, thread/TLS/park-wake/timer support and platform integration are
prerequisites for a real guest Tokio claim. UI work runs on its owning OS thread
through a current-thread runtime/local task set. Worker-safe I/O may use a
multithreaded runtime once the native worker-thread and synchronization services
exist. A thread-mobile owner is not permission to issue UI operations from an
arbitrary Tokio worker: resident WrongThread validation remains authoritative.
See Tokio's [runtime](https://docs.rs/tokio/latest/tokio/runtime/index.html) and
[LocalSet](https://docs.rs/tokio/latest/tokio/task/struct.LocalSet.html) contracts.

## Resident boundaries and cancellation

Implementation remains in System. SDK futures hold generation-checked operation
capabilities and owned application state, never resident manager references or a
lock guard across await. Request submission, readiness subscription, result
collection and cancellation must distinguish pending, committed and cancelled
operations. Dropping a future releases its waiter and requests appropriate
cleanup; it cannot claim that a concurrently committed write was rolled back.
Expose explicit awaitable cancellation/drain when an application needs confirmed
cleanup. Do not restart mutations just because a poll or transport wait retries.
This is the same rule used by filesystem provider tickets.

Source callbacks run on their owning application thread. A same-application drop
must be able to wait for a promised flavor while that source's producer executes.
For Rust this is event delivery plus a lazy closure invoked at most once; classic
callbacks are an adapter over the shared resident session. A promised-data take
is awaitable, including same-app delivery. Inline data completes on its first
poll. Only Dropped carries a take capability; tracking views expose metadata.

## Delivery and acceptance

1. Drag futures, lazy producers and completion wakes through the public SDK.
   Verify same-app delivery, inline completion, multiple pending takes, exactly-once
   production, typed failures, cancellation, dropped futures and waiter cleanup.
2. Add the reusable SDK event-driven executor and app startup integration with
   a std-compatible API. The early no_std drag pump is a bridge, not the final
   application entry-point contract. Verify unrelated event handling while a
   future waits, wake-driven polling, no nested queue consumers and responsive
   cursor/event timings. Keep the executor crate independent of kernel code.
3. Add awaitable file/provider operations and other services that can actually
   wait, using the same completion facility. Cover commit/cancel races and
   process-exit cleanup through real System services.
4. With the P8 std target/runtime, ship an allocating async std APPL and CLAP.
   Verify startup and default System-owned panic reporting without custom app
   runtime/panic boilerplate. A no_std-only demo is not this gate.
5. Qualify optional Tokio support on the actual guest target, beginning with the
   current-thread UI runtime, then worker-safe I/O. Test both executors against
   identical SDK conformance cases. Host-only Tokio execution is insufficient.

Status: architecture accepted; drag delivery implementation in progress. General
SDK executor, async entry points, guest std and guest Tokio remain delivery work.

Priority clarification, 2026-10-21: generic IPC and networking are optional later
work, not prerequisites for the first std/async applications. Prioritize
allocation/startup, files, clocks, OS threads/TLS/synchronization, event wakeups and
System-owned panic reporting. Compile the standard library's surface while
reporting unavailable facilities honestly; do not invent working sockets or
require a generic IPC subsystem merely to deliver resident completions.
