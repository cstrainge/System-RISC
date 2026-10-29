# Scheduler policy and control, v1

This allocation-free wire contract is consumed by the resident primary-only
SCHD/SCTL implementation and the optional Multi-Scheduler extension. Current
limits expose TAKEOVER and one active scheduling hart; MULTI_HART and PARK are
not exposed. The package is not installed by default. Source integration,
parsing bytes or advertising a feature does not establish native acceptance or
safe SMP execution. See [scheduling](../../SCHEDULING.md) for current mechanisms,
clock/configuration and exact qualification limits.

SCHD is an exclusive, kernel-only policy offer. The module has only irreducible
resident services, including SCTL and MLIF, and no module dependency. Its pure
policy callback uses private `Instance.dispatch` phase 6, operation CHOOSE=1,
with existing Host96/Instance24/Call64 and native ABI1. General broker entry to
SCHD/phase6 must refuse before dispatch. `Call.ticket` carries the per-hart
round sequence in this phase; it is not a DREQ or an ownership capability.

The core retains the genuine RuntimeBinding, ModuleExecution/source, Instance,
owner and policy generation. It builds the complete eligible set from canonical
records. No scheduler/registry guard, mutable frame or service borrow crosses
foreign entry. The callback cannot allocate, park, reenter services or do I/O.
Core enforces that scope and measures elapsed time around the restricted native
entry/return using the checked boot monitor timebase. The separately checked
platform quantum and callback allowance are exposed in LIMITS; they are not
interchangeable or latency guarantees. An otherwise valid late result is
discarded as an expired sample: bootstrap selection uses refreshed input/deadlines
for that round and retains the registration for a fresh sample. Malformed or
forbidden calls remain protocol failures; lateness cannot hide them. This does
not establish recovery from non-return or privileged faults. Attempted forbidden
native operations remain latched even if the author ignores their immediate
refusal. A per-call lease retains the actual code/Instance/plan through return.

## SCHD layouts

All integers are little endian. Round header is64 bytes:

| Offset | Value |
|---|---|
|0|`SCH1`|
|4|candidate record size, u32=32|
|8/16/24|policy generation / checked per-hart round / actual hardware hart, u64 each|
|32/40|actual monotonic microseconds / previous full thread incarnation or0|
|48/52|candidate count / Cause, u32 each|
|56/60|minimum / maximum quantum microseconds, nonzero u32|

Each32-byte candidate has u64 thread incarnation, runnable-since microseconds,
last hardware hart, then u32 flags and Kind. No unmeasured CPU-time value is
requested or fabricated.
Flags: interactive wake1, last-hart-present2. Absent last hart stores0. Hardware
IDs can be sparse/full-width. Candidates have nonzero canonical thread IDs,
not application IDs. Core owns uniqueness, complete-set construction, affinity,
actual readiness and running-claim exclusion. No truncation is allowed;
selection refuses an eligible set larger than its prepaid capacity instead of
truncating it. Maximum 4096 candidates bounds bytes and scans; it is not an OS
thread limit, CPU-time measurement or measured call-time guarantee. Current
runnable-since facts come from actual readiness transitions and last hart from
actual resume commitment. The current producer labels rounds Handoff; the Cause
enum does not imply that every listed cause is already produced.

Decision is48 bytes: `SCD1`, u32 version1, then u64 generation, round, hardware
hart, chosen thread (0=idle), and quantum microseconds. Idle requires an empty
eligible set and zero quantum. Selection requires membership and the supplied
quantum range. Codec checks only its immutable snapshot; core must revalidate
live generation/readiness/affinity/claim before committing or switching frames.
A stale, foreign or invalid decision leaves the bootstrap scheduler available.
Round exhaustion must refuse, never wrap. No wire identity is authorization.

## SCTL layouts and effects

SCTL v1 is a synchronous resident service. Every reply is i32 status +u32 payload
length +payload. Success status0; a negative status carries1..240 UTF-8 diagnostic
bytes without control characters. Caller supplies256..2048 reply bytes, checked
before any effect. Ordinary errors retain their actual cause.

|Operation|Input|Success payload|
|---|---|---|
|LIMITS1|empty|Limits64|
|PREPARE2|features u32, candidate capacity u32, topology epoch u64|Registration32|
|ARM3|registration generation u64|Registration32|
|HARTS4|generation u64, desired count u32, order u32=1 (ascending eligible hardware IDs)|Registration32|
|STATUS5|generation u64|Registration32|
|RETIRE6|generation u64, or0 for actual caller-owner cleanup after uncertain preparation|Registration32|

Limits64 contains version/features/candidate-capacity/max-active-harts (u32),
minimum quantum/maximum quantum/callback budget (u64), physical/active counts
(u32), boot hardware hart/topology epoch(u64). Features are TAKEOVER1,
MULTI_HART2, PARK4: exposed mechanisms, never a proof flag. Desired count always includes the actual primary, then the lowest N-1 eligible
other hardware IDs. Ascending order is an explicit package policy, not a hidden core
preference. Registration32 contains generation(u64), State(u32), last status(i32),
active harts/callback count(u32), transition sequence(u64).

PREPARE runs during initialization and preflights all fallible ownership/storage.
The validated Instance does not exist until entry returns; bind it through the
real RuntimeBinding afterward, before any shadow call. Retain the real provider
image/Instance with a central shared-facility registration owned by the actual
module. Failure cannot hide a published grant; uncertain effects remain retained
for owner cleanup. ARM records a takeover request; it does not acknowledge an
active policy or release a hart. HARTS is an intent while prepared/armed. The
current resident path accepts only
a count of 1 and refuses a count of 2 without partial activation. Other CPUs may already be
admitted to System idle; that is separate from permission to dispatch authored
code. Additional dispatch admission remains behind the core gates.

After initializer return, core runs one shadow decision from a genuine core
boundary, validates it, atomically changes active policy and confirms MLIF
startup. Author MLIF CONFIRM and synchronous commit must refuse SCHD
publication without the actual core handover proof. Never call policy from MLIF
CONFIRM on an author stack. No general
application/module task may execute on peers until the actual dispatch gate
passes. Partial admission/rollback failure retains exact per-hart preparations
and cause; it cannot report the requested count as active or silently substitute
fewer CPUs.

RETIRE first revokes further policy decisions and restores bootstrap policy.
Maintenance withdraws the actual offered interface and waits for its matching
canonical completion acknowledgment. Retired also requires real callback
quiescence, no remaining code-dependent plan/Instance readers and a primary-only
dispatch floor; only then may those pins and registration backing be released.
Clean withdrawal reports MLIF Refused/OPERATION_CANCELLED while SCTL preserves a
clean last status. Fault withdrawal retains its cause; no new MLIF wire phase
is implied. Busy retains the original grant and ticket. Quiescing-owner status/
retirement uses the original retained cleanup authority, never new admission or
a fabricated owner. A dropping SDK controller must report retained state rather
than claim disposal. Fault containment and arbitrary callback non-return remain
separate native gates.
