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

Primary references: Apple's [PostEvent documentation](https://dev.os9.ca/techpubs/mac/Toolbox/Toolbox-66.html)
and [Event Manager overview](https://dev.os9.ca/techpubs/mac/Toolbox/Toolbox-56.html).
The original executable probes resolve mask, overflow and bitmap details in this
specific installed system; the manuals alone are not the runtime oracle.
