# Process isolation

Status: design/backlog, tracked as [P13](CORE-TOOLBOX-ROADMAP.md#p13--process-isolation).
Current applications and System share trusted M-mode memory. Owner checks and
generation-tagged IDs reject invalid service access, but do not stop arbitrary
native loads/stores or machine code. This work establishes hardware-enforced
protection for APPLs and CLAPs, including Terminal and hosted development tools.

## Address spaces and privilege

Use the RISC-V [supervisor and user privilege model](https://docs.riscv.org/reference/isa/priv/supervisor.html)
with an Sv39 page-table implementation and a narrow machine-mode firmware layer.
Move System execution and traps to the appropriate privileged kernel boundary;
user apps run in U-mode. Simply setting satp while retaining today's M-mode app
execution is not process isolation. Specify delegation, timer/interrupt entry,
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
physical addresses or loading raw ELF. Shared immutable code is optional later;
private writable state is mandatory. JIT/translated code needs controlled
write-to-execute publication, never arbitrary kernel executable mappings.

Scheduler switches select the correct address space and thread kernel stack.
Specify ASID/TLB reuse, unmapping and page reclamation ordering. Add cross-hart
shootdowns and migration rules with P1; page reuse cannot precede outstanding
translations or operations. Shared generation-tagged IDs and separate ownership
checks remain required alongside the MMU.

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

P6 extensions and P14 loadable drivers intentionally remain in-kernel trusted
code. DRVR metadata validation is an ABI/contract gate, not a sandbox or proof of
correct implementation. Their fatal faults retain the kernel policy; this plan
does not claim apps can survive arbitrary corruption inside a driver/kernel.
Prevent user apps from directly programming MMIO/DMA or supplying unchecked DMA
addresses. Device/DMA mappings are owned by trusted kernel adapters; broader
hardware/IOMMU threat profiles are separate gates.

## Delivery and acceptance

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

Use private guest fault fixtures plus canonical public-SDK service assertions in
`crates/mac-toolbox/tests`. Prove cross-app and kernel reads/writes, execute-data,
write-code, forged requests, stale IDs, wrapped/page-boundary pointers, unmap/I/O
races and stack overflow cannot corrupt System or a surviving observer app.
Verify dying parents/workers cannot leave DMA/IPC/callback references to freed
memory. Repeat reuse/termination under contention and multiple harts. Page-table
shape tests alone are insufficient: run actual U-mode code and fault probes,
then launch apps and exercise GUI, terminal, disk and network regressions.
Record memory/switch/syscall cost and UI latency; isolation must not reintroduce
polling, event storms or full-scene repainting.
