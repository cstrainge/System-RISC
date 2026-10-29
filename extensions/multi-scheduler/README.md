# Multi-Scheduler

Source implementation against SCHD/SCTL v1. The real resident SCTL consumer and
final scheduler wiring remain pending; this package is not installed by default
and has no runtime/SMP acceptance yet. Missing SCTL must produce actual admission
refusal, leaving the resident boot scheduler active.

The initializer defers MLIF readiness, obtains genuine core limits, prepares an
owner-scoped registration, requests ascending eligible hardware IDs and arms a
handover. Core binds the validated actual Instance after initializer return,
performs a shadow decision from its own safe boundary, and confirms readiness
only after real policy commit. No callback runs from MLIF CONFIRM.

The module implements the safe Driver::schedule entry with stateless priority
bands/aging. Policy never allocates, blocks, logs, locks the controller or calls
a service. Kernel mechanisms retain frames, identities, running claims,
preemption, topology, device maintenance and quiescence. No secondary author
execution is authorized merely by a feature bit or by this package existing.

Quiesce retains its original controller on Busy/refusal; positive retirement
requires actual callback quiescence and the primary-only bootstrap floor.
A privileged fault/non-return is not claimed recoverable. See the
[wire/core obligations](../../crates/mac-scheduler-interface/README.md) and
[policy rules](../../crates/mac-scheduler-policy/README.md).
