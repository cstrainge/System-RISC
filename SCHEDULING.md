# Scheduling policy and resident mechanisms

System owns thread identity, saved state, stacks, readiness, running claims,
interrupts, context switches and maintenance. A loadable scheduler chooses among
immutable snapshots supplied by those mechanisms. Policy decisions cannot grant
execution authority or replace the live frame/claim checks.

The resident SCHD/SCTL path supports replacement policy and concurrent native
application/module-worker execution on admitted CPUs. LIMITS exposes TAKEOVER and,
when more than one CPU was discovered, MULTI_HART with that topology's maximum.
PARK is not exposed. Hardware discovery and System-idle startup alone grant no
author execution; the active policy and core admission turn must establish it.
The normal image installs the
[Multi-Scheduler package](extensions/multi-scheduler/README.md) in the blessed
Extensions directory. Without an active policy, boot scheduling remains on the
primary. The bounded native observations and remaining gates below distinguish
implementation from qualification.

## Preparation, publication and retirement

An eligible extension declares exactly one exclusive, kernel-only SCHD offer,
no module dependencies and only SCTL/MLIF resident services. During its actual
initializer it defers MLIF readiness, obtains SCTL limits, prepares a registration
under its genuine module owner, requests an exposed hart count and arms the
handover. The resident preflights a complete candidate buffer, packet buffer,
checked round sequence and bounded diagnostic slots for every discovered CPU,
under the original module Kernel account. PREPARE is restricted to that initializing, deferred owner; it is
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
withdraws every peer's dispatch eligibility and posts real target work. A
DRAINING peer still enters the scheduler: it cannot resume its old author merely
because new author selection is forbidden. Only that target's complete captured
turn can release its running claim, select its genuine affine idle frame/context
and publish the drain receipt. A monitor interrupt ACK is not that receipt.
Retirement also requires no active callbacks, no outstanding per-CPU buffer loans
or reports, no remaining plan/binding readers and every target drained before
releasing code-dependent policy state. Retired is not published merely because
a counter was set to zero. Owner cleanup
retains its unique ticket on refusal. Clean withdrawal marks MLIF Refused with
OPERATION_CANCELLED while SCTL retains a clean last status; a failed policy keeps
its failure cause. A stopped owner's retirement uses its original retained
cleanup authority, not new admission or a fabricated owner.

## Real facts and the callback boundary

System copies the complete eligible set from canonical THRD rows after current
capture and deadline/wake updates. The snapshot clock is sampled inside that
same scheduler turn, so another CPU cannot publish a newer readiness timestamp
between an earlier clock read and the copied facts. Each candidate carries its full thread
incarnation, actual runnable-since microseconds, optional last hardware hart,
interactive-wake flag and kind. Runnable age changes at a real readiness
transition and resets when execution is committed; repeating a no-change update
does not make a thread younger. Last hart records actual resume commitment.
These facts contain no CPU-time estimate: switch counts and wall time are not
fabricated execution accounting.

The admitted wire capacity is 4096 candidates. PREPARE reserves storage for the
complete supported set independently for each CPU. Loans and round sequences are
CPU-local and bound to the real immutable row; concurrent callbacks cannot share
packet storage. If a later eligible set exceeds that capacity, policy
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

After foreign entry ends, System retains the original policy/plan pin and sampled
generation through the final frame commitment. Under the scheduler lock, it
holds that same active registration's short phase exclusion across live thread
identity, readiness, affinity and claim validation and the actual claim/frame
mutation. No foreign call, allocation or owning disposal occurs in that joint
turn. Owning selection pins drop after those guards. A stale choice, withdrawn
generation or ordinary Busy is contention: bootstrap rechecks live state instead
of permanently refusing a healthy policy. A first protocol refusal is sticky
before a contended phase turn, so it cannot permit another entry to the faulty
callback. Peer failure reports use prepaid bounded slots; primary maintenance
performs firmware logging after all snapshot/registration guards have ended.

## Placement and primary work notification

Ordinary application threads and Automatic workers default to the actual
core-admitted eligible set. No application opt-in or permanent primary pin is
imposed. A worker may explicitly request Primary for a genuine restriction.
Canonical System idles retain their separate actual hart affinity. Bootstrap,
policy candidates and final frame commitment all check the genuine dispatch
phase and temporary service obligations; discovery or a hardware number cannot
grant execution.

An active policy's HARTS request can reduce or expand its count within the checked
limits. The core preflights the retained hardware-readiness receipts before
publishing a change, always retains the primary and selects peers in checked
hardware-ID order. Reducing the count requires target-owned draining; a later
HARTS request can repromote those targets under the same active generation.
RETIRE removes that registration's grant. It is not permission to PREPARE or ARM
the same already-bound instance again.

## Primary service routing

The display, physical input, opaque firmware and current resident service bodies
remain primary-owned. An application ECALL on a peer records a temporary primary
obligation before borrowing its request, entering a logical trap scope or
performing effects. The AP captures the actual THRD's full frame and retains the
original ECALL PC/registers. It releases the real claim before waking the primary.
The primary claims that same thread and restores the same owned stack/frame at
the unchanged ECALL. The service then executes once in its ordinary implementation.
Only final result/PC commitment clears that attempt's obligation; genuine
deferred I/O keeps it through its existing completion/replay protocol.

Direct module Host functions use one private System binding to acquire a
canonical primary-service lease before caller pointers, request views, manager
or critical guards and author thunks. Policy denial runs first. The primary
fast path checks placement and mints its lease in one masked metadata turn;
peer acquisition migrates the same real continuation. Nested lease depth and
an ordinary ECALL obligation are independent overlays on base affinity. No
closure RPC, copied caller context or fabricated executor is involved.

A process stopped while a Host lease exists retains that actual primary-affine
continuation until its final lease drops. Pending retirement wakes existing waits
and refuses new suspension before it can strand that guard. Explicit EXIT cannot
abandon a held lease. The final owned Core pin leaves after the scheduler guard
and before local interrupt restoration. Structural ownership uncertainty keeps
the original row, backing and ticket quarantined with its typed cause, rather
than acknowledging destruction or repeatedly polling permanent retention.

## Work notification and idle

The primary receiver is admitted with SSIE while global interrupts remain
masked, then published after the actual first scheduled frame commits. Peers use
their actual retained readiness receipts and startup-installed SSIP receiver.
Real callable-policy, retirement, readiness and claim-release changes publish
coalesced hints; guard-free outer returns post the existing monitor transport.
Primary-only work does not broadcast to peers. Target-local checked wake epochs
are durable before MMIO, and only the actual captured target's SSIP acknowledgment
clears a posted notification. Work ownership stays in the real
queues and cleanup tickets. A typed transport refusal retains those obligations
and records its first cause without an allocation or retry/log storm.

Readiness already included in a complete policy snapshot or a fresh bootstrap
choice discharges its unposted runnable hint under the existing scheduler lock.
A new ready edge during foreign policy execution retains its own hint. This
avoids an extra interrupt for work the current turn has already observed;
it does not acknowledge a posted IRQ or unrelated cleanup work.

An idle peer has no local author quantum armed. It waits for genuine readiness,
dispatch admission or terminal work rather than scheduling periodically with an
empty queue. The primary retains its periodic input/maintenance and global wait
deadlines. This is not a measured latency or indefinite no-lost-wake guarantee.

### Earlier primary receiver observations

Private optimized one- and four-CPU guests observed the real primary Joined2
receiver acknowledging epoch1 after actual policy startup and epoch2 after an
application launch; its SecondaryStart stayed Prepared0. Real policy retirement
advanced the primary request and acknowledgment from1 to5 in both configurations.
The same artifact exercised peer idle/startup acknowledgments and post-launch
instruction publication, native refusal/retirement cases and subsequent UI use.
A same-sized prior ROM was refused by the changed monitor fingerprint with a
rebuild diagnostic, before expanded monitor views. Two initial diagnostic setup
failures left the now-default policy installed; System rejected the competing
exclusive offer. Those attempts did not reach their intended callback cases;
the corrected private installations did.

This artifact's System resource fork is
`eea547c73768fae5ccf670388b59320396834e329da0881fc8048b2311e5a35d` and ROM
`430dfaeb04be0d644a0207a9112dea7e57d6af9f2ccb6018b6bf8193d58c95f9`.
These observations precede the72-byte worker descriptor and public CPU-query
changes. Periodic primary maintenance remains. They do not establish indefinite
idle/no-lost-WFI, concurrent author dispatch, all multiwriter races, arbitrary
MMIO-fault recovery, latency or full SMP. Their primary-only scope is not evidence
for the later routing, joint claim or target-drain implementation described here.

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

## Combined native development observations

A later optimized RV64/RVA23 `no_std` build includes the definition/worker
admission scope and the module image-pin preemption correction. Its System
resource fork is SHA-256
`ee94aa8676bd9fb023966c28819fccf85f8909cc3ce00be02cf4b806a6f0b824`,
System ELF `0cad4e6b78beb5cbc9840fa0aad9b867e9369271ca90bb5a7a27d5e7bd525ffb`
and ROM `9984bec4eb70f1aee786e4b18e724a081d46ddb8b4a9368c00be73d058a61126`.
Private QEMU virt guests used 8 GiB RAM, 800×600 and tracing disabled, with the
production loader, trap entry, allocator and scheduler:

- Two independently loaded applications and two Automatic workers were each
  observed in their unique native bodies on multiple physical CPUs with the
  same stack. Snapshots also showed overlapping bodies. Guest register, service,
  cancellation and join checks completed, both worker reclamation receipts
  appeared, and following input was observed.
- The policy diagnostic completed four-to-one demotion, same-generation
  repromotion and retirement, with actual target idle PC/SP observations,
  frozen/resumed peer callbacks, stale-generation refusal and three worker
  reclamations.
- Normal and failed-publication worker scenarios reclaimed their four and three
  workers respectively. Declared Primary and Automatic worker bodies each
  submitted four null/zero-size ordinary Toolbox requests and observed
  WrongThread before borrowing; actual Host operations and LOG continued.
- With the real Host Clipboard worker and normal Multi-Scheduler active, 130
  Get Info open/close cycles, outside-release cancellations, About/Close and
  following mouse input completed without image-registry, menu-feedback or
  Finder-action refusal markers. The vdagent hardware was isolated and had no
  host clipboard peer; this does not establish clipboard sharing.
- Separate normal one- and four-CPU boots opened Reactor Room, processed input,
  quit through the application's own shortcut and observed subsequent input
  and genuine primary wake acknowledgments.

Earlier combined builds exposed an actual image-registry Busy refusal during
Get Info and folder rendering. Source review found that preemptible Host calls
could acquire the outer image-registry guard before its inner alias gate masked
interrupts. The module execution-pin lookup now masks before outer acquisition
through guard disposal. It performs only bounded lookup, reader retention and
execution-pin acquisition; module callbacks, executable writes and publication
remain outside that turn. The failed guest identified registry Busy; it did not
capture the exact suspended holder. The later 130-cycle observation does not
prove every possible interleaving. A first-refusal diagnostic remains bounded
and preserves the typed cause.

These are development observations of the exact paired artifacts, not general
SMP, timing or race-absence guarantees. Read-only paused register snapshots,
phase summaries and paced input affect timing. Separate guests ran concurrently
for the body/UI captures and for policy/worker captures; that host load is not
an SMP stress measurement. The direct-worker refusals do not exercise malformed
or foreign-surface requests within a real definition scope. Asynchronous device
reset, sparse topology, all failure/interruption paths, full OOM recovery and
complete extension/account closure remain unqualified. The independent control
and menu changes require observations on their combined build as well.
Arbitrary privileged-fault containment remains **unproven**, M-D04's
**required future gate**; resource containment through the walk is distinct.

## Initial native SMP observations and remaining gates

The bounded SMP observations used optimized RV64/RVA23 `no_std` artifacts in
private QEMU virt guests with four physical CPUs, 8 GiB RAM and tracing disabled.
The System resource fork was SHA-256
`67eebc78d932cb4209ae51027f9a64b6cd808d05c0db417e395e9fac052a436c`;
its ELF was `5d5062d2f1894ceee560da931c5fb3580f7126aca7387a98c53304ceed427481`
and ROM `9984bec4eb70f1aee786e4b18e724a081d46ddb8b4a9368c00be73d058a61126`.
The guest used the production trap, scheduler, allocator, normal application and
module loader paths; no host-linked service substitute supplied these results.

Two independently loaded applications and two Automatic module workers completed
guest integer/FP/vector/CSR, memory, file/resource/window and Host-service phases.
Read-only physical CPU PC/SP snapshots correlated unique body symbols with actual
relocated entries and canonical creation records. Each of the four bodies was
observed with the same stack on different physical CPUs. One snapshot contained
all four bodies on four distinct CPUs. Guest cancellation/join completed, both
actual worker reclamation receipts were observed and following input was usable.
Body completion or aggregate CPU residency alone would not establish placement.

A separate normally loaded policy diagnostic observed callbacks on all four CPUs,
live HARTS1 demotion, target-owned affine idle PC/SP observations, HARTSN
repromotion under the same genuine generation and eventual RETIRE. Peer callback
counts stayed fixed during demotion and resumed after repromotion. Retirement
included actual target idle observations, stale-generation refusal and three
canonical worker reclamations. A solo normal-worker run independently observed
four actual reclamations and following input on this same paired System/ROM.
Final rebuilt normal and rollback module scenarios observed all four and three
respective canonical reclamations, descriptor refusals and following input.

These are exact-artifact, bounded QEMU observations. Paused register snapshots,
phase summaries and paced input have observer cost and do not establish absence
of races, worst-case latency, fairness, sustained pressure behavior or other
hardware. Sparse hardware IDs, arbitrary transport/fault injection, all pending-
retirement interleavings, full OOM recovery and complete extension/account closure
remain unqualified. General native application-thread creation, per-domain CPU
time and process privilege isolation remain separate work. Definition-specific
entry/failure behavior requires its own target scenarios; the SMP workload does
not qualify every definition callback or panic path. **M-D04:** the walk provides
resource containment. Arbitrary privileged-fault containment is **unproven** and
is a **required future gate**.

## Earlier primary-only observations

The following historical observations belong to their stated earlier artifacts;
they are not additional qualification of the SMP artifact above.

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
Those captures installed the package explicitly; the normal image builder now
includes it. That build-list change is not additional native runtime evidence.

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

Multiple physical CPUs at admitted idle did not establish concurrent author
execution in those earlier runs. The later bounded SMP observations above add
actual body placement/migration, service and policy-drain scenarios; they do not
retroactively widen these older captures. General application-thread creation,
CPU-time attribution and sustained concurrency/pressure qualification remain
separate work. **M-D04:** the walk provides resource containment. Arbitrary privileged-fault containment is **unproven** and is a
**required future gate**.

See [THREADING.md](THREADING.md) for execution/lifetime rules and the
[SCHD/SCTL wire contract](crates/mac-scheduler-interface/README.md) for layout
and SDK obligations.
