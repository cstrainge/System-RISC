# Scheduling policy and resident mechanisms

System owns thread identity, saved state, stacks, readiness, running claims,
interrupts, context switches and maintenance. A loadable scheduler chooses among
immutable snapshots supplied by those mechanisms. Policy decisions cannot grant
execution authority or replace the live frame/claim checks.

The resident SCHD/SCTL path is implemented for **primary-only takeover**. It
exposes TAKEOVER, one active scheduling hart and no MULTI_HART or PARK feature.
Other discovered CPUs may complete their genuine System-idle startup and wake
acknowledgment; this does not authorize application or module bodies there.
The [Multi-Scheduler package](extensions/multi-scheduler/README.md) is optional
and is not installed by default. Native qualification remains limited as stated
below; source integration is not runtime acceptance.

## Preparation, publication and retirement

An eligible extension declares exactly one exclusive, kernel-only SCHD offer,
no module dependencies and only SCTL/MLIF resident services. During its actual
initializer it defers MLIF readiness, obtains SCTL limits, prepares a registration
under its genuine module owner, selects one hart and arms the handover. The
resident preflights complete candidate/packet storage under the original module
Kernel account. PREPARE is restricted to that initializing, deferred owner; it is
not a way to register another policy from an already bound running instance.

The canonical registration retains its real owner, source/execution image,
validated Instance, policy generation and plan. After successful entry return,
System binds the returned Instance. ARM and worker activation do not themselves
publish an active policy. At a genuine scheduler boundary, System calls a shadow
round, checks the returned decision and revalidates the selected native thread.
Only that path can issue the sealed handover proof required for SCHD interface
publication and MLIF confirmation. A package cannot make itself active by calling
MLIF CONFIRM from its own stack.

Typed rejection keeps bootstrap selection available. Returning malformed output
or attempting a forbidden service withdraws the registration from further policy
entry and retains the real retirement obligation. An otherwise valid result
that exceeds its elapsed-time allowance is an expired sample: System discards
it, refreshes input/deadlines and uses bootstrap selection for that round while
retaining the policy for a fresh round. Guest wall time includes host pauses;
expiry alone does not establish module corruption. Protocol validation precedes
this distinction, so a slow malformed result is still a protocol failure.
SCTL replies preserve typed status and bounded UTF-8 diagnostic causes. A caller
must retain its Controller through Busy, Retiring or other unacknowledged cleanup;
a request or guard Drop is not a disposal acknowledgment.

RETIRE first prevents new policy leases and restores bootstrap selection.
Maintenance withdraws the actual SCHD interface, drops its payload outside the
registry and waits for the matching canonical completion acknowledgment. It then
requires no active callbacks, no remaining plan/binding readers and the actual
primary-only dispatch floor before releasing code-dependent policy state.
Retired is not published merely because a counter was set to zero. Owner cleanup
retains its unique ticket on refusal. Clean withdrawal marks MLIF Refused with
OPERATION_CANCELLED while SCTL retains a clean last status; a failed policy keeps
its failure cause. A stopped owner's retirement uses its original retained
cleanup authority, not new admission or a fabricated owner.

## Real facts and the callback boundary

System copies the complete eligible set from canonical THRD rows after current
capture and deadline/wake updates. Each candidate carries its full thread
incarnation, actual runnable-since microseconds, optional last hardware hart,
interactive-wake flag and kind. Runnable age changes at a real readiness
transition and resets when execution is committed; repeating a no-change update
does not make a thread younger. Last hart records actual resume commitment.
These facts contain no CPU-time estimate: switch counts and wall time are not
fabricated execution accounting.

The admitted wire capacity is 4096 candidates. PREPARE reserves storage for the
complete supported set. If a later eligible set exceeds that capacity, policy
selection refuses and uses bootstrap selection rather than truncating candidates.
This bounds packet size and traversal; it is not a 4096-thread OS limit, a timing
qualification or a fairness guarantee.

The private callback uses the normal native Instance dispatch boundary with
SCHD policy phase6 and CHOOSE. It is not a general broker request: no DREQ is
created and its round field is not an ownership ticket. A callback lease pins
the actual source/execution, Instance and admitted plan through return. No
scheduler/registry guard, mutable trap-frame borrow or scratch-buffer lock spans
foreign execution. The input snapshot is immutable and the output is a separate
fixed-size decision buffer. System validates the complete call envelope and
exact generation, round, hardware hart, selected member and quantum on return.

A per-CPU restricted scope refuses allocation, deallocation, reallocation,
allocation-scope changes, service/broker entry, resolution, worker operations and
module critical sections through native gateways. An attempted forbidden
operation latches a refusal even if the author ignores its immediate result and
returns an otherwise valid decision. Pure policy therefore cannot park, log,
lock its Controller or perform I/O. SDK infallible allocation failure is not a
recoverable allocation probe; do not test it as an ordinary returned denial.
Trusted privileged code that bypasses the gateways is outside this contract.

After foreign entry ends, System rechecks the actual policy registration and
chosen thread's live identity, readiness, affinity and running claim. The final
claim/frame commitment is shared with bootstrap selection. Current author
execution remains primary-only with interrupts masked across this commitment.
This is not a cross-hart atomic policy/claim commit contract.

## Clock, quantum and callback allowance

The platform definition owns separate checked fields:

| `platforms/QemuRISC-V.json` field | Current selection | Meaning |
| --- | --- | --- |
| `scheduling.quantum_microseconds` | 250 | Current minimum and maximum runnable-thread quantum |
| `scheduling.callback_budget_microseconds` | 1000 | Allowance checked after the private policy call returns |

The build validates these fields and generates resident constants from that one
record. SCTL LIMITS exposes the actual compiled selections. Change the platform
and rebuild System and its packages together; do not add a second constant in
an extension or assume the quantum equals the callback allowance.

Clock conversion uses the checked boot monitor's actual DT timebase frequency.
Runnable timestamps use checked tick-to-microsecond conversion; elapsed callback
time rounds up and chosen quanta convert to ticks with checked upward rounding.
No fixed 10 MHz assumption is introduced by this path. Invalid conversion or
sequence exhaustion refuses rather than wrapping or manufacturing time facts.

The measured callback interval surrounds the restricted native policy entry and
return, including its SDK dispatch and scope guard. Executor/resource overlay
setup and subsequent live-decision validation are outside that interval. This is
elapsed guest-counter time, not author CPU accounting; machine-mode work and
virtualization effects are not separately attributed. A late returned callback
is discarded without committing its choice. Only the first expired sample per
registration is logged; ordinary sampling does not produce a per-tick log.
The allowance is not a watchdog, preemption deadline, mouse
latency bound or recovery mechanism for a callback that never returns.

## Native observations and remaining gates

Optimized RV64 `no_std` artifacts ran in private QEMU guests with one and four
physical CPUs, tracing disabled and the normal boot, loader, trap and allocator
paths. Application and module bodies remained primary-only. The observed System
resource fork was SHA-256
`7b77aa781a52ae43268a9a99cee64f6f2b5693d1dafe798e3f2336b4223cd9d4`;
its ELF was `750c11222cc9aa3c2afa7dba22bc8747711fb94d38a3d2f88af1fd950b89b139`
and ROM `8f45ef9ffa90aeb523fa3619ce458c0889db690600d865c2a63d59413a034d67`.
The final formatting rebuild produced the same byte-identical System resource
fork and ROM. Its companion ELF is
`29b3989e945e8a091e4afd74f8131cc4f481930653edd1c0520ca9e489607b0c`;
the resource loader consumes the packaged segments, not ELF debug metadata.

The ordinary optional Multi-Scheduler package completed core-confirmed startup
on both configurations. Reactor Room opened, processed mouse input and quit
through its own application shortcut; following mouse input was observed. This
establishes that scenario, not a mouse-latency bound or whole-OS acceptance.
The package is still not part of the default installation.

The existing native module diagnostics additionally observed:

- Actual Prepared/Pending with zero callbacks; early author MLIF CONFIRM returned
  ServiceBusy without changing that registration. A committed worker then armed
  the real shadow handover, leading to Active.
- Malformed PREPARE, undersized replies with no effect, wrong generation,
  a caller without SCHD and unsupported two-hart requests returned typed causes.
- A three-second native Active interval enclosed real Reactor/mouse activity.
  RETIRE reached actual canonical withdrawal acknowledgment, stale STATUS was
  refused, and two newly created bootstrap workers completed and were reclaimed.
  This is not proof of whole-module account closure or registration-slot reuse.
- A callback that ignored a safe service gateway's AccessDenied still failed its
  restricted scope. A separately packaged callback that changed the actual call
  ticket after valid SDK dispatch failed BadAbi. Neither reached Active; both
  retained their exact MLIF failure, retired, refused stale STATUS and ran the
  same subsequent worker/UI scenario.
- A synchronous initializer returning success without DEFER/PREPARE was rejected
  with ServiceBusy before policy publication; the desktop and subsequent Reactor
  launch/exit remained usable. That observation does not establish every rollback
  destructor or privileged-fault recovery.

Valid samples exceeding the 1000-microsecond allowance were observed at 1031 and
2942 guest microseconds; bootstrap handled the expired round while the same
registration remained usable. The exact cause of a long interval was not
observed. No per-decision host acknowledgment serialized these native workloads.
Only phase/failure summaries were logged; that observation still has overhead.

Earlier failures are retained: a cold 855-microsecond return exceeded the initial
250-microsecond allowance; another run exposed an over-strict diagnostic assertion
when retirement completed between reads; and a 7386-microsecond return exposed
the old mistaken permanent-fault classification of valid expiry. The allowance
was separated from the quantum, the diagnostic now admits real progress, and
valid expiry now discards only that sample. An initial Finder power ServiceBusy
also exposed a reachable Runtime metadata preemption window in source review;
the exact interrupted holder was not captured. Later native configurations with
the metadata IRQ guard observed usable Finder power capabilities.

Host compilation and host-linked Rust suites are not evidence for these paths.
These observations do not establish every chosen-frame commitment, fairness,
worst-case responsiveness, all callback gateway variants, full OOM cleanup or
recovery from a nonreturning policy.

Multiple physical CPUs at admitted idle do not establish concurrent author
execution. Full SMP still requires a joint policy-generation/claim commitment,
real service/device/firmware affinity or routing, retirement/quiescence across
executing harts, wake/progress qualification and sustained native overlapping
workloads. General application-thread creation, migration and CPU-time
attribution remain separate work. **M-D04:** the walk provides resource
containment. Arbitrary privileged-fault containment is **unproven** and is a
**required future gate**.

See [THREADING.md](THREADING.md) for execution/lifetime rules and the
[SCHD/SCTL wire contract](crates/mac-scheduler-interface/README.md) for layout
and SDK obligations.
