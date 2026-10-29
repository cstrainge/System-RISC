# CPU information

Ordinary applications can enumerate discovered CPUs and read their selected
state and activity through the public Toolbox. No inspector grant or scheduler
extension is required:

```rust
let cpus = system.inspection().cpus()?;
let count = cpus.len();
for ordinal in 0..count {
    let cpu = cpus.at(ordinal)?;
    // cpu.hardware_id, cpu.state, cpu.dispatch_eligible, cpu.activity
}
```

The count includes every enabled CPU discovered at boot. Ordinals are positions,
not hardware IDs, object handles or execution authority. Membership is immutable
for the current boot. Without an active scheduler, the primary executes the OS
and applications while admitted peers select their System idle contexts. With
Multi-Scheduler active, core-approved eligible peers can execute ordinary apps
and Automatic module workers. Loading a package or reading CPU metadata does
not itself grant dispatch permission; actual policy/core admission owns it.

Each row distinguishes discovery, admission, idle, executing and stopped states.
Executing includes resident work. The eligibility flag describes the actual
core gate; it is independent of the hardware count. Rows are observed separately,
so enumeration is not a simultaneous whole-machine snapshot.

Activity is either typed unavailable information or cumulative idle/executing
residency in the admitted DT clock's ticks. `ticks_per_second()` supplies that
actual frequency. For two samples of the same CPU, `after.activity_since(before)`
returns checked deltas; `executing_fraction()` converts a nonempty delta to a
fraction. Clock regression, overflow, uninitialized measurement and an unknown
stop timestamp remain explicit unavailable states. No switch count is converted
into pretend CPU time, and no per-process CPU attribution is implied.

The measurement boundaries are serialized complete-capture and resume-commit
observations, not cycle-accurate WFI/sret. The remaining save/restore tail, machine
mode work and virtualization effects are not separately attributed. Queries have
observer cost. A stopped CPU has no known stop timestamp in the current monitor
contract, so totals are not extrapolated past that unknown point.

Counters update at actual class changes, with an unchanged-class early-out.
Each CPU owns its small guarded state; queries lock only the requested row.
Ordered native clock reads serialize memory with the time CSR as required by
[RISC-V Zicsr](https://docs.riscv.org/reference/isa/unpriv/zicsr.html).
There is no new timer, per-input log or per-event whole-CPU scan.

The native family is `CPU_INFO` outside the classic trap range. Its dedicated
payload boundary gives unknown operations and malformed fields a CPU-specific
bounded diagnostic, without inferring a payload family from its size. Stale
native packages fail the unreleased layout fingerprint; rebuild the whole world.

The initial native qualification used optimized RVA23 artifacts on QEMU virt
with one and four physical CPUs. An ordinary diagnostic application without
inspector permission enumerated every CPU with the scheduler extension removed,
observed coherent compute/wait residency deltas and idle peers, and checked five
informative malformed-request refusals. Separate runs with the normally installed
primary-only policy exercised application launch/exit and subsequent input.
The paired worker descriptor exercised refusal, normal execution, rollback and
actual reclamation in that same System build.

Those initial CPU-query observations alone do not qualify concurrent application
execution. Later exact-artifact app/worker migration and policy-drain observations
are recorded in [SCHEDULING.md](../SCHEDULING.md); topology/activity rows alone are
not evidence of which thread ran where. No timing bound or hardware outside QEMU
is qualified by those scenarios. Clock regression, counter overflow,
unknown-stop handling and uninitialized-state branches were not fault-injected.
They do not establish whole-System correctness or per-domain CPU accounting.
