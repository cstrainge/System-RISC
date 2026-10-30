# Native application runtime and allocation

Native RV64 applications use the `mac-toolbox` application feature and one safe
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

## Startup and ownership

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
