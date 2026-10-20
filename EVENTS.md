# Event Manager captured slice

The public `System.events()` facade enters the resident Event Manager through
`EVENTS` (`0x1001a`). `NEXT_EVENT` retains its existing application event routing.
Both are guarded by the resident desktop state; the host test adapter owns an
independent manager in each service session. There is no unsynchronized global
event queue in the SDK and no SDK hardware decoder.

The accepted original profile is English/Roman System 7.6.1 on the pinned q800
reference. Original queue and physical-input captures, source/build/ROM/System
hashes, launch commands and isolation checks are retained under
`crates/mac-toolbox/tests/data/events/`. The canonical assertions are
`tests/conformance/events.rs` and `tests/events.rs`; the same queue function runs
against the host resident service and real disk-booted RV64 ECALL service.

| Contract | Public Rust interface | Captured scope |
| --- | --- | --- |
| PostEvent/PPostEvent value semantics | `os_post` | Types 0..15; current tick/global mouse/modifier snapshot; disabled type returns original `evtNotEnb` 1; capacity 20 discards oldest and succeeds |
| OSEventAvail | `os_peek`, `os_event(mask, false)` | First matching entry; repeated inspection leaves order unchanged |
| GetOSEvent | `os_get`, `os_event(mask, true)` | Removes only first matching entry; unrequested entries retain order; queued null event is available |
| FlushEvents | `os_flush` | Removes matching entries before first stop-mask entry; stopping entry remains even when it also matches flush mask |
| SetEventMask | `set_os_mask`, `os_mask` | Controls low-level posting independently of retrieval masks; native initial mask is explicitly all 16 bits |
| GetKeys | `keys`, `input_state` | Physical 128 virtual-key bitmap; byte `code/8`, LSB bit `code%8`; A and Shift+A compare complete original maps |
| Button | `button`, `input_state` | Physical left button; classic modifier bit 128 means released |
| StillDown | `still_down` | Released physical button always false; while held, first queued mouse-up makes result false without consumption; first mouse-down or empty queue leaves result true |
| Global pointer state | `mouse`, `input_state` | Global screen coordinates; original `GetMouse` is port-local, so it is not the alias of this native method |
| TickCount | `ticks` | Unsigned wrapping 60 Hz machine ticks; native `uptime_ticks` retains the unwrapped value |

The low-level OS queue receives physical packets before menu/window/modal
routing. It does not receive generated native menu commands, window activation,
or update events. Posting a keyboard/mouse event does not change physical input
state. The physical ECALL test installs an actual MENU resource and confirms
that Command+A reaches the Menu Manager while the raw key transition remains in
the OS queue.

`post`, `peek`, and `flush` operate on the separate existing native application
queue consumed by `next_event`. This explicitly native queue has 128 entries,
preserves caller-supplied record fields, and discards its oldest on overflow.
`peek` examines queued entries only; pending window updates are synthesized by
`next_event`. Kind 14 means MenuCommand in this native queue and remains an
application-defined event in the low-level OS queue. They are distinct namespaces
despite sharing the record representation.

The current virtio host adapter maps Ctrl to Mac Command. It omits the original
ADB device address in key-message bits 16..23; physical key comparisons retain
the virtual-key/character low 16 bits. Its Caps Lock toggles on press, while the
captured q800 profile releases its caps state with the physical key. These are
explicit device-profile differences, not accepted original parity. Pointer
origins differ between the two machines; posting tests compare each device's
current state, rather than asserting one machine's absolute startup coordinate.

Still open: classic A-line/register/Pascal-stack adapters, PPostEvent queue-element
pointers, event codes outside 0..15, original autorepeat timing,
WaitNextEvent sleep and mouse-region semantics, application
event ownership/suspend/resume, high-level events and timers. Full application
threads and per-application event ownership remain the THREADING.md scheduler
gate. Shared service guards preserve that implementation path but do not claim
that scheduling exists today.

## Planned extended pointer input

Status: backlog; the current hardware decoder handles the left button only and
does not decode wheel input. Track delivery in
[roadmap P3](docs/CORE-TOOLBOX-ROADMAP.md#p3--extended-pointer-input).
Right-button and wheel support must extend native services without changing the
classic application's input contract.

- Keep the classic 16-byte EventRecord, event numbers, masks, message fields and
  modifier meanings intact. `Button`, `StillDown` and `btnState` continue to
  describe only the primary button. Extra-button transitions must not generate
  classic mouse-down/up events, change primary-button polling, or interfere
  with primary-button double-click/drag tracking. Preserve left-click behavior.
- Provide a separately versioned native event contract through the Toolbox,
  with explicit button identity/state, signed horizontal/vertical scroll deltas,
  defined units/precision, pointer position, modifiers and monotonic timestamps.
  Do not squeeze extra event kinds into the existing 16-bit retrieval masks or
  reuse reserved classic event bits. Expose feature discovery and subscription;
  keep future pointer devices possible without making applications decode evdev.
- Decode device packets once into hardware-independent input. Preserve ordered
  button edges and signed wheel values, including negative values, high-resolution
  residuals and horizontal scrolling. Avoid counting both coarse and high-resolution
  reports of the same movement. Reconcile held state on device removal/capture
  loss without leaving buttons stuck or releasing another device's held button.
- Route to the eligible window/view under the pointer, respecting application
  ownership, modality, activation and active tracking/capture. Define those
  policies explicitly, including nested scroll views and keyboard-focus behavior.
  A resident control may consume a wheel action or delegate it to its app, but
  one action must not scroll twice. Secondary clicks request context actions in
  supporting native UI; they do not implicitly invoke primary-button actions.
- Legacy applications receive only their classic event projection by default;
  unsupported extra input is ignored. System-owned dialogs and scrollable
  controls can handle extended input internally through semantic operations.
  Arbitrary legacy custom views require explicit, verified compatibility support;
  do not infer a scrollbar and synthesize clicks or arrow keys. Any optional
  right-click/Control-click mapping must be an explicit compatibility policy,
  separate from the current host Ctrl-to-Command keyboard mapping.
- Standard native scroll boxes, TextEdit, Standard File, Finder and ResEdit
  should adopt wheel handling through their shared services. Context menus use
  the resident Menu Manager and resource-based commands, with application
  participation; a secondary click alone cannot reveal arbitrary app actions.
- Accumulate compatible adjacent wheel deltas within a bounded budget, retaining
  units, target and ordering relative to button/key/focus transitions. Preserve
  edge events under wheel load and bound work per drain. Ignore zero/no-change
  input and repaint only changed content or controls; never recompose merely
  because a pointer packet arrived.

Acceptance: public SDK conformance tests for both native delivery and unchanged
classic records/polling/masks; right/left chords, drags, capture, modal and
cross-app routing; wheel direction, precision, limits, horizontal/nested scrolling,
no duplicate handling, unplug cleanup and sustained-load fairness. Compare the
classic projection to retained original-system primary-button captures. Add a
translated 68k event/polling probe when the adapter exists. Private QEMU tests
must demonstrate usable Finder/ResEdit scrolling and context actions, bounded
repainting, responsive cursors and measured sub-millisecond dispatch latency
under the declared test load; host display latency is measured separately.

Apple's [Event Record reference](https://leopard-adc.pepas.com/documentation/mac/Toolbox/Toolbox-45.html)
defines the classic boundary. Extended pointer semantics above are System RISC
design requirements, not claims of original System 7 behavior.

Primary references: Apple's [PostEvent documentation](https://dev.os9.ca/techpubs/mac/Toolbox/Toolbox-66.html)
and [Event Manager overview](https://dev.os9.ca/techpubs/mac/Toolbox/Toolbox-56.html).
The original executable probes resolve mask, overflow and bitmap details in this
specific installed system; the manuals alone are not the runtime oracle.
