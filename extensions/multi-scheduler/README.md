# Multi-Scheduler

The normal image installs this package in `System Folder/Extensions`. It uses
the resident SCHD/SCTL v1 mechanism to request the checked eligible CPU maximum.
Current limits expose TAKEOVER and, for a multi-CPU topology, MULTI_HART. PARK is
not exposed. Missing SCTL or rejected preparation leaves primary boot scheduling
available. Actual app/Automatic-worker execution and migration have bounded native
QEMU observations; full race, latency and fault qualification remains open.

The initializer defers MLIF readiness, obtains genuine core limits, prepares an
owner-scoped registration, requests the checked maximum hart count and arms a
handover. Core binds the validated actual Instance after initializer return,
performs a shadow decision from its own safe boundary, and confirms readiness
only after real policy commit. No callback runs from MLIF CONFIRM.

The module implements the safe Driver::schedule entry with stateless priority
bands/aging. Policy never allocates, blocks, logs, locks the controller or calls
a service. Kernel mechanisms retain frames, identities, running claims,
preemption, topology, device maintenance and quiescence. No secondary author
execution is authorized merely by a feature bit or by this package existing.

Quiesce retains its original controller on Busy/refusal; positive retirement
requires actual interface withdrawal/acknowledgment, callback/reader quiescence
and target-owned drain receipts returning every peer to its real affine idle.
An active controller can reduce then expand HARTS under the same generation;
RETIRE does not permit re-PREPARE of its already-bound Instance. The checked
platform quantum and callback allowance are separate LIMITS values; the policy clamps its preferred quantum
to those actual limits. Its source defaults are not a response-time guarantee.
An otherwise valid expired sample is discarded for refreshed bootstrap selection
without removing the registration. Invalid replies and forbidden operations
remain protocol failures; this does not recover from a nonreturning callback.

Earlier optimized native package observations completed core-confirmed startup
in private QEMU with one and four physical CPUs. Reactor Room opened and processed mouse input,
then quit through its own application shortcut; subsequent mouse input was
observed. All author execution remained primary-only. Separate existing native
diagnostic packages exercised actual retirement, stale status, forbidden-service
and malformed-envelope refusal and subsequent worker/UI progress. They do not
establish this package's complete quiesce/account closure or full SMP.
Later exact-artifact scenarios observed two applications and two Automatic workers
migrate with the same stacks across four CPUs, including overlapping body PCs,
real service/cancel/reclaim activity, and policy demotion/repromotion/retirement
with target-owned idle observations. Their pauses and summaries perturb timing;
no race-absence or complete account-closure claim follows. The current
250-microsecond quantum and independent 1000-microsecond callback allowance do
not establish a latency bound. See the
[resident contract and exact observations](../../SCHEDULING.md),
[wire/core obligations](../../crates/mac-scheduler-interface/README.md) and
[policy rules](../../crates/mac-scheduler-policy/README.md).
Arbitrary privileged-fault containment remains M-D04's required future gate.
