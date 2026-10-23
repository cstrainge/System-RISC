# TextEdit typing and event delivery

## Current scheduler and batching

Native Finder, QuickDraw Test and Text Edit use scheduler-backed event
waits. The OS samples readiness every 500 us and prioritizes fresh foreground
input. Nonblocking event reads return ready input before painting. TextEdit
drains at most 128 consecutive keyboard events before applying their edits and
committing one frame. A nonkeyboard event, including a menu shortcut, ends the
batch without being consumed. Releases need no editor or menu work.

The compositor retains at most 2 MiB of actual committed desktop/window pixels.
Cache misses use the original renderer and original resource owner; pending
scenes stay private until EndUpdate. RAMFB uses a bulk RAM copy. The fixed desktop
checkerboard is generated from two repeated rows, avoiding full-screen pattern
evaluation before restoring a cached desktop. Window drag/grow tracking uses
reserved compositor scratch storage, keeping its full-screen mask off the
8 MiB heap shared by the resident managers and application stacks. Drag outlines
visit structure-piece edges instead of scanning the whole desktop for borders.

Canonical tests include 20 isolated edits (20 commits), a 20-key queued burst
(one commit), the 128-event bound, ordered menu-command boundaries and physical
clipboard/close shortcuts. `event_latency` separately checks masks, timeouts,
posted Null, pointer wakeup, exactly-once physical keys and progress alongside a
non-yielding application. Strict sub-ms timing is opt-in with a traced image;
see [event tracing](EVENT-TRACING.md). Physical traces start at driver decoding,
not host arrival. EndUpdate pixels and visible host presentation are separate
latency measurements. Cold synchronous services can still delay observation.

## Measured acceptance for this milestone

Sequential traced captures `latency-ui-final3-linux` and
`latency-ui-final3-windows` each pass all 31 event scenarios. Against the previous
`b52ff50` milestone, the 20-character burst improves as follows:

| Measurement | Linux QEMU | Windows QEMU |
| --- | --- | --- |
| Previous input to final text frame | 756 ms | 2,022 ms |
| Current input to final text frame | 19.2 ms | 21.4 ms |
| Maximum decoded-key delivery in that burst | 0.453 ms | 0.493 ms |
| Same delivery with two background apps | 0.350 ms | 0.437 ms |

The final default-image Windows capture observes 84 ms for host injection
through the final acknowledged 20-character edit, versus 2.27 seconds before
this milestone. That host interval includes QMP/WSL transport; the traced frame
intervals above start inside the guest.

The standalone wait fixture also passes the opt-in 1 ms threshold: maximum
physical delivery is 0.523 ms on Linux and 0.624 ms on Windows; 64 queued samples
per workload also stay below the threshold. Waiting for 100 ms reschedules the
blocked application once, rather than rotating it continuously. Strict reports
are `../results/system7/latency-strict-final3.json` and
`latency-strict-final3-windows.json`. Work-count and timing comparisons are
`latency-final3-{linux,windows}-comparison.json`.

These are sampled driver-decode-to-delivery results, not a hard real-time bound.
An earlier Windows strict run failed on a 1.11 ms first physical-key sample;
its capture and log remain in `event-latency-windows-140348` and
`latency-final-windows.log`. Final GUI traces still observe cold masked services
up to 65 ms on Linux and 98 ms on Windows. Such work can postpone input before
its first trace record. Moving or dividing long synchronous rendering/loading
work is still required before claiming bounded latency under arbitrary load.

## Earlier measurements

The previous TextEdit milestone (`570f4f5`) did synchronous unrelated work
between successive event reads:

- Every consumed keyDown refreshed all Finder menu policy, including three File
  Manager calls to find Trash. KeyUp fell through to the same full update.
- Every successful menu request marked the compositor dirty, including getters
  and writes that preserved the existing enabled/checked state.
- Every input packet, including a plain key or modifier transition, marked the
  desktop dirty before `next_event` returned its queued event. The compositor
  repainted the old scene before the application could respond to the key.

A private RV64 probe including the actual Finder sources measured20 edits:
31 public60-Hz ticks for editing/drawing and28 for menu policy (about0.52 and
0.47 seconds), with60 File Manager calls. A private physical20-character burst
on that build took about1.44 seconds. After the changes, the same size burst
in a headless VM took about0.59 seconds, and the profiling workload reports zero
filesystem calls and zero measured menu ticks. These are host timing
observations, with16.7-ms guest clock granularity, not universal latency claims.

The same20-key burst using headless Windows QEMU went from about2.49 to
1.47 seconds. Its profiling guest also reports zero filesystem calls and zero
measured menu ticks. The visible GTK frontend was left untouched during these
measurements.

The application now updates only Edit-menu policy after text input. It refreshes
browser/Trash policy after actions that can change it. Resident menus report
whether their visible state changed, and keyboard packets preserve compositor
cleanliness while still updating physical input, queues, modifiers and timestamps.
The existing end-update presentation contract remains intact. Full-scene composition was still a cost at that milestone when text or a caret
changed. The current changes above reduce replay and per-key commits.

Native cursor movement uses a separate TextEdit operation rather than changing
captured TEKey semantics. Finder supports:

| Keys | Movement |
| --- | --- |
| Arrows | Move the caret; left/right collapse an existing selection |
| Shift+arrows | Extend or shrink selection around a fixed anchor |
| Home / End | Start/end of the visual line |
| Shift+Home / Shift+End | Extend selection to the line boundary |
| Ctrl+Home / Ctrl+End | Start/end of the document |
| Ctrl+Shift+Home / Ctrl+Shift+End | Extend selection to the document boundary |

Vertical navigation retains the desired horizontal position across short lines.
UTF8 boundaries, wrapped-line affinity, an empty final CR line and direction
reversal have shared public SDK tests. Ctrl is the current host mapping for the
Mac Command modifier.

Reproduce from the workspace with:

```sh
cargo xtask image
cargo test --locked -p mac-toolbox --test typing -- --ignored
```

Rust owns acceptance in `crates/mac-toolbox/tests/typing.rs`; Python only packages
private disks, injects keys and collects complete acknowledgements. The result
is `../results/system7/typing-checks.json`. The private profiling guest forwards
real resident requests through the supplied gate and includes the actual
Text Edit editor/menu/batch sources. Tests do not inject input into the user's visible VM.

To run the same canonical assertions using the installed Windows QEMU binary:

```sh
SYSTEM7_TYPING_WINDOWS=1 cargo test --locked -p mac-toolbox --test typing -- --ignored
```

This uses headless Windows guests on their own monitor port4457; the visible
native desktop continues on4447. Its report is `typing-windows-checks.json`.
