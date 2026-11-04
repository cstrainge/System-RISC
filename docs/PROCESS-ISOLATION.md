# Process isolation

Status: implementation and target qualification in progress, tracked as
[P13](CORE-TOOLBOX-ROADMAP.md#p13--process-isolation). Native applications enter
U-mode with their own Sv39 roots and private writable image, heap and stack
mappings. System and kernel modules remain privileged. This is an implementation
statement, not a claim that every callback, cleanup or failure path is qualified.

The current isolation configuration exposes four physical CPUs but admits only
one active hart. Multi-hart isolation, shootdown and reclamation require their own
qualification before that cap is removed. Native user-fault observations apply to
the specific guest artifacts and scenarios exercised; they do not establish
application compatibility, account closure, arbitrary privileged-fault recovery or
correctness on other hardware. Host Rust test results are not OS acceptance.

## Address spaces and privilege

Use the RISC-V [supervisor and user privilege model](https://docs.riscv.org/reference/isa/priv/supervisor.html)
with an Sv39 page-table implementation and a narrow machine-mode firmware layer.
System already executes behind its supervisor trap boundary; isolated user apps
must run in U-mode. Setting satp while retaining S-mode application execution is
not process isolation. Specify delegation, timer/interrupt entry,
kernel stacks and firmware calls together, following the
[machine-level contract](https://docs.riscv.org/reference/isa/priv/machine.html).

Define architecture-neutral address-space/map/protect/copy-user interfaces for
future x86, with narrow unsafe page-table/trap adapters. Give each process private
writable data/BSS, heap, stacks/TLS and argument/environment storage. Workers
share only their owning process's address space. Keep kernel/firmware memory,
page tables, devices and framebuffer inaccessible to apps; shared surfaces and
IPC buffers require explicit mappings and ownership rules.

Map code read/execute, constants read-only and data non-executable; add stack
guards and allocation/commit limits. Separate native preferred virtual addresses
from physical placement, allowing app instances to use the same virtual layout.
Reuse validated compact segments/RLOC rather than assuming reservations are
physical addresses or loading raw ELF.
Executable projects do not accept application placement fields. The checked
platform supplies one application link origin (4 MiB on QemuRISC-V); packaging
records only the actual page-rounded ELF memory extent in NABI. Both firmware
and System reject an incompatible link origin. NABI/RV64 remain required loader
contracts; ResEdit archive mode preserves those raw resources verbatim. Rebuild
all native packages/media together after a project-format change, retaining
native ABI 1. This build contract alone is not runtime qualification.

Shared immutable code is optional later;
private writable state is mandatory. JIT/translated code needs controlled
write-to-execute publication, never arbitrary kernel executable mappings.

Scheduler switches select the correct address space and thread kernel stack.
Specify ASID/TLB reuse, unmapping and page reclamation ordering. Add cross-hart
shootdowns and migration rules with P1; page reuse cannot precede outstanding
translations or operations. Shared generation-tagged IDs and separate ownership
checks remain required alongside the MMU.

## Process lifetime and migration boundaries

Admit a canonical, generation-scoped ProcessSpace before thread publication and
retain genuine execution pins in thread state. Its tables and branch pages are
owned independently; supervisor aliases retain the actual System/module backing,
while application writable frames belong only to their Process. Modules need
publication and retirement across all registered roots. Space teardown must
release mapping pins and await real translation completion before returning table
pages; it cannot wait for an image retirement gate that already waits for space
resources to disappear.

Final trap return must select the root belonging to the committed outgoing
thread after Rust guards and borrows have ended. Restoring the interrupted root
unconditionally would return a new thread through another Process's page tables.
Internal firmware-call root scopes retain their separate caller restoration rule.
An intermediate private-root stage may retain trusted S execution, but must be
labelled accordingly and must refuse missing application space rather than
falling back to the System root.

Private user heaps and stacks require user virtual reservations separate from
retained kernel aliases and physical receipts. Zero every newly exposed physical
page before granting user access, including rounded heap extent bytes outside
an individual allocation request. Address-wait authentication must follow the
actual Process mapping instead of assuming every address is a physmap alias.
User entry, child-thread startup and callbacks then need user CRT trampolines,
a process-readable descriptor without resident callback pointers, and checked
request/buffer copying. Clearing SPP alone is unsafe.

## Syscalls, resources and callbacks

Replace exposed resident pointers/function entry references with user-callable
stubs and versioned checked requests. Validate address ranges, permissions,
overflow, alignment and lengths at copy-in/out. Do not keep borrowed user Rust
references or unchecked pointers across waits, callbacks or address-space
switches. Snapshot requests and pin/copy buffers according to explicit rules;
concurrent mutation/unmapping must not become a kernel alias or memory-safety bug.

No app can forge AppId, ThreadId, Terminal/session authority or another process's
handles. Descriptor/IPC/map identities use the shared generation facility.
Resource/file/image reads must copy or map validated immutable data rather than
return kernel addresses. Specify cancellation, exit and pending-I/O ownership
before reclaiming buffers or process pages.

Audit all executable-resource callbacks. System's trusted definitions and P6
kernel extensions may execute in kernel context; app-provided WDEF/CDEF/MDEF,
ResEdit editors and legacy cdev code must not gain privilege merely because a
manager calls them. Define user-mode callback dispatch/marshalling, drawing
surfaces, reentrancy and lifetimes without resident manager locks spanning calls.
Translated 68k apps get per-process legacy globals/low-memory views and checked
classic pointer/handle conversion, not access to actual kernel memory.

## Faults and trust boundary

An invalid user access, illegal/privileged instruction or stack fault stops the
affected process and its workers, cancels owned work and invokes System-owned
reporting from trusted state. Copy panic/fault details safely; the dialog/event
loop cannot depend on corrupted app code or a still-running process. Invalid
syscall pointers return defined errors rather than crashing the kernel.

The access-fault presentation uses the same System-owned bomb dialog as an
application panic. It identifies an application access fault separately from a
processor exception, based on the architecture's captured fault identity rather
than application-supplied prose. The report includes the canonical application
name, cause, program counter, fault address and captured registers in a scrollable
view, with Copy Report and Close Application controls. The System owns drawing,
input handling and dismissal; the failed application's code handles none of them.
An application-supplied diagnostic cannot replace the captured hardware tail.

P6 extensions and P14 loadable drivers intentionally remain in-kernel trusted
code. DRVR metadata validation is an ABI/contract gate, not a sandbox or proof of
correct implementation. Their fatal faults retain the kernel policy; this plan
does not claim apps can survive arbitrary corruption inside a driver/kernel.
Prevent user apps from directly programming MMIO/DMA or supplying unchecked DMA
addresses. Device/DMA mappings are owned by trusted kernel adapters; broader
hardware/IOMMU threat profiles are separate gates.

## Delivery and acceptance

The QemuRISC-V platform retains four physical CPUs but selects
`scheduling.maximum_active_harts = 1` until the new U-mode application paths
receive their SMP qualification. The checked build rejects zero or a cap above
the configured CPU count. Runtime policy limits are additionally bounded by
actual discovered CPUs, and core admission enforces the cap before changing
dispatch eligibility. Idle peer readiness and physical CPU inspection remain
available. Raising the configured cap requires a rebuilt platform and does not
establish that the qualification below has passed.

1. Privilege/boot/interrupt transition and kernel mappings, guarded physical-page
   ownership and checked firmware/device access on one hart.
2. Private app address spaces, user entry/exit and context switching, loader
   mappings, allocator/TLS/stacks and a protected syscall transport.
3. Migrate service buffers, surfaces and user callbacks; integrate APPL/CLAP/std
   process creation, session IPC and fault/exit cleanup. Keep legacy translation
   and low-memory compatibility explicit.
4. SMP TLB/reclamation, shared-memory lifetime and adversarial concurrency tests
   with P1. Optional shared code/COW/lazy paging can follow; fork is not required
   for the first isolated Command/process API.

Acceptance must execute actual target applications and the production kernel,
including the real allocator, traps, loader, scheduling and paging configuration.
A host-linked Rust suite does not qualify those paths. Existing finite native
captures apply only to their recorded artifacts and scenarios; new diagnostic
machinery follows the repository's agreed validation work order. Exercise
cross-app and kernel reads/writes, execute-data,
write-code, forged requests, stale IDs, wrapped/page-boundary pointers, unmap/I/O
races and stack overflow cannot corrupt System or a surviving observer app.
Verify dying parents/workers cannot leave DMA/IPC/callback references to freed
memory. Repeat reuse/termination under contention and multiple harts. Page-table
shape tests alone are insufficient: run actual U-mode code and fault probes,
then launch apps and exercise GUI, terminal, disk and network regressions.
Record memory/switch/syscall cost and UI latency; isolation must not reintroduce
polling, event storms or full-scene repainting.

## Definition painting and cancellation

An application-owned definition runs through its process's user callback thread.
The resident compositor retains an owned staging buffer and a window/phase/clip
cursor. Preparing a pixel packet, running the author and retiring the original
reply occur outside resident manager guards. Capture and accepted pixel replay
use fresh, bounded Canvas borrows after validating the original object, source,
geometry, z order and relevant committed content revisions. Cached pixels do not
replace that semantic validation. Window geometry cannot overtake an admitted
paint callback.

Cancellation retains the same admitted callback until an actual broker return or
stopped-source result. Dropping an observer is not author completion or a resource
ACK. A stale render transaction discards its staging changes, restores every
attempted target clip from the committed base and retains its exact damage for a
subsequent pass. It must not replay an accepted author prefix merely because a
later phase suspended. Callback refusal and transient manager-lock contention
remain distinct causes, including when both project to a Busy wire status.

The ordinary committed base remains available to System-owned reports and cursor
overlays while application drawing waits. Close and Zoom tracking invalidate the
definition's own button bounds. Geometry changes still have their separate scene
invalidation. Callback drawing, compound controls, source-stop cancellation,
original-ticket closure and target performance require explicit guest coverage;
these design and source boundaries alone do not qualify those paths.

Managed control measurement, child hit-testing, layout, scrolling and splitter
tracking retain owned calculations and the original request through user author
waits. Automatic layout precedes the actual native positioned-event, ordinary
event and wait/poll transports. Its completed prefix is retired before event
waiting; a retained completion marker prevents replaying that prefix. Clean
event polls inspect coalesced owner state rather than scanning control trees.
Publication checks the original object keys, tree, definitions and geometry;
tracking additionally checks that the actual Window is still active.

Grow feedback is a reversible present-time overlay. The same WDEF first
calculates the proposed structure; GROW is clipped to that validated footprint.
The previous accepted mask stays owned until its frontbuffer pixels are erased.
Grow feedback never becomes ordinary scene content and does not require copying
or clearing a full-screen mask for each pointer movement.

Caller-owned pixel drawing uses the original checked Surface buffers and mapping
pins. An affine loan identifies that particular request and its actual Process
account. User author calls draw into private staging; only a successful complete
traversal may commit pixels and scalar output. Cancellation or refusal discards
staging and returns the same buffers before the existing final copyout. An
offscreen SBOX retains its own real object readset; it is not represented as a
fabricated Window. Protected System storage does not fall back to ordinary
Process authority for application-provided authors.
