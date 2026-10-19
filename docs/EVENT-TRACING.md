# Event amplification investigation

The native runtime delivered each physical key transition once in the captured
typing scenarios. The main amplification was in activation, damage handling and
presentation work around those events. `WM_STATE` is a revision log, not a count
of events delivered to applications.

## Evidence and fixes

The baseline is commit `bb2306b` with opt-in tracing added, before the behavioral
changes. Captures run private Linux QEMU and Windows QEMU instances with cloned
disks; the interactive desktop receives no test input. Artifact hashes, launch
commands, serial logs, bounded binary snapshots and decoded records accompany
each capture under `../results/system7/events-*`.

The completed baselines are `events-baseline-linux-2/capture.json` and
`events-baseline-windows/capture.json`; reviewed captures are
`events-reviewed-linux-2/capture.json` and `events-reviewed-windows/capture.json`.
Both reviewed captures pass the aggregate SDK trace acceptance. The comparison
summaries are `events-linux-comparison.json` and `events-windows-comparison.json`.
Earlier incomplete or intermediate attempts are retained separately and are not
the acceptance evidence.

On Windows, the captured isolated key went from two window commits to one;
reselecting Finder went from three commits and activation/update traffic to zero
commits and no such events. QuickDraw launch observed40 full compositions before
and19 after. Phase settling differs and caret timing varies, so these counts
are observations backed by the separate exact-contract regressions, not timing
thresholds. The 20-key batches had20 key-downs and20 key-ups in both versions.
Instrumented batch throughput did not improve consistently; the remaining
rendering/polling costs below still need work.

| Finding | Change | Canonical SDK acceptance |
| --- | --- | --- |
| Activating a multiwindow app selected each window in turn, emitting intermediate activation pairs. Background creation, desktop selection and exit had related paths. | Raise the window plane and select its final window or desktop atomically. Preserve relative order and do not activate background windows temporarily. | `processes.rs` / `process_cases.rs`: 28 additional contracts across two child lifecycles. |
| Repeated foreground activation and unchanged window visibility invalidated every window. | No-op activation, hide/show and visibility changes preserve damage and event state. | Process acceptance acknowledges damage, repeats requests, then checks that no extra Activate or Update appears. |
| Finder rebuilt its desktop and every folder for each activation notification. | Retain existing content; service Window Manager damage through Update. | Actual Finder application UI and original pixel gates. |
| A thread could present another window after BeginUpdate cleared its live drawing list and before EndUpdate finished it. | Stage replacement commands and port state separately; EndUpdate publishes the entire scene. Pending commands do not dirty committed pixels. | `retained_update.rs`: two actual OS threads, seven checkpoints, 122,500 exact interior pixels. |
| An editor key drew the hidden reset caret, then the first Null/TEIdle drew again to show it. | Prime TEIdle before the app commits drawing; retain original TextEdit pre-idle semantics. Avoid unchanged grow-clip writes and redundant idle state queries. | `typing.rs`: 20 isolated edits plus first idle passes require exactly 20 commits and zero repeated grow-clip calls; original caret and navigation gates remain. |
| Stationary clipboard thumb tracking redrew on every Null; unrelated window activation also redrew the viewer. Consumed Nulls also queried disk-backed menu policy. | Draw only when the scroll value changes; Window Manager Update owns activation damage. Skip menu policy for consumed Nulls. | `clipboard.rs`: 100 physically held stationary tracking polls require zero commits, File Manager calls and menu calls; unchanged release and unrelated activation require zero commits; physical movement must change scrolling. |
| Idle event queries copied foreground state into/out of the caller, even without input or presentation work. | Return early when no decoded input or presentation work exists. Box each app's EventManager so real switches exchange pointers instead of copying its inline queues. | Real physical modifier/key/selection checks, process isolation, preemption and graphical regressions. |

Application-posted synthetic events retain private provenance. A foreign window
number in a posted event does not authorize routing it into that window owner's
queue. No blanket event deduplication or event dropping was introduced.

## Scenario coverage

The trace transport exercises 31 phases: idle Finder/editor/three-app states;
folder open; launch and relaunch; isolated text, return and navigation; two
20-character bursts; selection chords and modifier-only input; pointer movement;
text selection; window drag/grow; app switching and repeated current-app choice;
menu cancellation; hide/show and repeated Show All; quit and resumed input.

The broader regression gate adds held/cancelled/reentered drag, grow, zoom and
close interactions; first-click activation and Command-drag; menu resources and
shortcuts; modal dialog behavior; raw/native event masks, peek and flush; Scrap
changes; and a non-yielding application preempted by the OS timer. These are
bounded scenarios, not an exhaustive proof for every event producer or future
application. Host-generated autorepeat cadence, queue saturation under prolonged
load, additional hardware devices, SMP and future blocking waits remain outside
this trace campaign.

## Reproduce

```sh
SYSTEM7_EVENT_TRACE=1 CARGO_INCREMENTAL=0 cargo xtask image
python3 scripts/capture_system7_event_trace.py --output ../results/system7/events-local
SYSTEM7_EVENT_TRACE_CAPTURE=../results/system7/events-local \
  cargo test --locked -p mac-toolbox --test event_trace -- --ignored
python3 scripts/capture_system7_event_trace.py --windows-headless \
  --output ../results/system7/events-windows-local
SYSTEM7_EVENT_TRACE_CAPTURE=../results/system7/events-windows-local \
  cargo test --locked -p mac-toolbox --test event_trace -- --ignored
```

The Windows capture uses private headless port4457. The visible desktop uses
port4447. Do not run two Windows captures on4457 simultaneously. Run timed
comparisons sequentially, without competing compilation or private VMs.

The Rust trace acceptance checks ordered, once-only key transitions, no artificial
activation/damage for no-op app choices, modifier-only behavior, no idle Finder
composition, no native queue-drop reports and no overwritten records. It rejects
phases that fail to reach a quiet gap. `cargo xtask check` includes the independent
process, clipboard, typing and retained-update SDK regressions without requiring
the diagnostic feature.

## Trace format and measurement limits

`mac-system/event-trace` adds a bounded in-memory trace, not a public application
trap. Initialization prints its address and byte length once. The transport
pauses only its owned VM, reads the bytes with QMP `pmemsave`, then resumes it.
The default build omits the ring, counter locks and diagnostic clock reads.

Version1 contains six little-endian u64 header words (magic, version, sequence,
record capacity, app buckets, service buckets), 16 x32 x4 service counters,
16 x16 event counters and 8192 records of eight u64 words. Records contain
sequence, 10MHz timestamp, operation kind, app identity and four operation fields.
App counters0..14 are individual identities;15 aggregates higher identities.
Actual app IDs remain present in ring records. This campaign uses IDs1..4.

Outer and nested trap counts/durations are separate. Outer duration includes its
nested calls; render duration is also included in the surrounding service. Do
not add these overlapping durations. Null polls and presentation/input checks
have cumulative counters but no per-poll ring records. Keys, routing, native
delivery, synthesized Updates, window mutations, activation and compositions
have ordered records. `peek` is distinct from consumption.

An Update repeats while damage remains unacknowledged; it is synthesized from
window state rather than repeatedly enqueued. BeginUpdate acknowledges that
damage. The raw OS event queue is a separate API surface from the routed native
queue. Its retained physical events do not show that the app handled a key twice.

A composition while a window update is pending is legitimate when it displays
the last committed scene. The `frames_while_update_pending` diagnostic measures
opportunity for interference; only the pixel test establishes scene atomicity.
Phase wall time includes input transport, settling, snapshots and unrelated
caret blinks. The summarizer reports input-to-final-key-frame timestamps and work
counts separately. Instrumented timings are observations, not portable latency
thresholds.

## Remaining performance work

The final default-image `cargo xtask check` passes, as do host/RV64 Clippy,
trace-feature Clippy and the separate Windows typing/navigation acceptance.
The latter observed a20-character burst of2.27 seconds,84 guest60Hz ticks for
the isolated edit/draw profile and1 tick for its menu-policy work. These timings
do not establish that typing throughput is fixed; they locate substantial work
outside menu policy and duplicate key delivery. Reports are
`../results/system7/toolchain-checks.json` and
`../results/system7/typing-windows-checks.json`.

Applications still busy-poll nonblocking NextEvent. Fast paths reduce the cost
but do not implement a blocking WaitNextEvent or an idle scheduler thread.
Full-scene composition replays visible retained content and copies the entire
640 x480 framebuffer inside an interrupt-masked service. Those long services and
per-key diagnostic serial output can still delay input and other apps. Scheduler
wait/wakeup and measured damage/caching work are separate next steps; a cache
must preserve global pattern phase, clipping, resource ownership and heap limits.
