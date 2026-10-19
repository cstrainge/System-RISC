# Finder typing and event delivery

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
The existing end-update presentation contract remains intact. Full-scene
composition is still a cost when text or a caret actually changes.

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
real resident requests through the supplied gate and includes the actual Finder
editor/menu sources. Tests do not inject input into the user's visible VM.

To run the same canonical assertions using the installed Windows QEMU binary:

```sh
SYSTEM7_TYPING_WINDOWS=1 cargo test --locked -p mac-toolbox --test typing -- --ignored
```

This uses headless Windows guests on their own monitor port4457; the visible
native desktop continues on4447. Its report is `typing-windows-checks.json`.
