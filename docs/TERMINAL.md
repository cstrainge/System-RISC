# Terminal and CLAP applications

Terminal, Cli Extension, Shelly and echo are installed by the `qemu_riscv`
platform. This is an initial usable implementation, not full POSIX terminal or
PTY compatibility. Sub-second shell startup and the shared async runtime remain
unfinished; the intended async contract is in [ASYNC-SDK.md](ASYNC-SDK.md).

## Launch and ownership

Terminal is a normal std APPL. Cli Extension claims executables whose Macintosh
file type is `CLAP`; each command runs in its own real process and OS thread.
Execution extensions query the executable information they need. The native
default handler is tried after registered handlers. A CLI process does not
register an application menu entry or own a GUI menu bar.

Finder routes a CLAP open to Terminal, which creates another window for that
command. Opening Terminal normally starts Shelly. Multiple Terminal windows own
separate sessions. Session authority comes from System's authenticated Terminal
host and owned window, not an environment variable or a claimed process name.
A failed launch leaves the existing window usable and retains the caller's
endpoints. GUI applications retain the default stub-stream behavior.

System attaches the selected stdin, stdout and stderr pipe ends only after
execution/session admission succeeds. Endpoints are owned, generation checked
and transferred to the child as part of launch publication. Rust
`std::process::Command` supports checked CLAP creation, piped streams, blocking
wait, output collection and exit status. This does not imply that every std
process option, stream-inheritance mode or termination operation is supported.
Unsupported operations return errors.

## Installation and shell defaults

- Terminal: `Applications:Utility:Terminal`.
- Shelly and echo: `Applications:Utility:Cli`.
- Terminal supplies the CLI executable search path. It does not import the host
  computer's PATH.
- Shelly starts in the user's Documents directory; `~` names that directory.
- Shelly reads `.shelly_init.shy` from Preferences.

Shelly is an implicit source fork under `vendor/shelly`, with its upstream
identity and license retained. Networking and other facilities unavailable on
System RISC remain unavailable. `echo` is a separate native CLAP and receives
real argv through the platform startup contract.

## Input, output and rendering

The terminal engine parses input/output and owns buffering and terminal state;
the renderer owns its view of that text. The initial profile includes UTF-8 text,
VT cursor operations, ANSI foreground/background colors, alternate-screen state,
a blinking cell-width box cursor and a 2000-line scrollback buffer. The managed
right scrollbar bounds the text viewport. Only damaged paint regions are drawn.
These implemented pieces do not establish complete xterm, Unicode grapheme,
full-screen TUI or sixel protocol compatibility.

Child stdout and stderr use their own byte streams and the terminal's styles;
stderr is not automatically painted red. Terminal's own diagnostics can use an
error style. Reedline controls its prompts and syntax highlighting. Shelly
asks for device attributes at startup and draws its prepared sixel logo when
Terminal reports sixel support and a cell size. A screen keeps
any number of pictures within a count and pixel budget, oldest removed first.
Each stays on its text line, scrolls into history with that line and is drawn
again when history is browsed. An erase or a partial scroll that covers a
picture removes it. The cursor is left on the line below a picture. This path
is unqualified on the guest.

The QEMU key mapping uses left Alt for Command, Ctrl for Control, and right Alt
for Option. This preserves terminal control keys while keeping host-intercepted
Windows keys out of application shortcuts. Future hardware mapping is separate.

The native Crossterm input adapter uses a per-call pipe read deadline. It does
not toggle the shared endpoint mode or wake every four milliseconds to test an
empty pipe. A read timeout is `WouldBlock`, not EOF, and leaves the caller's
buffer unchanged. Ready bytes and real EOF take precedence over expiry. See
[PIPES.md](PIPES.md) and the public `PipeReader::read_wait` contract.

## Closing and cleanup

Closing a session sends stdin EOF and keeps the output readers and original
child receipt until output EOF and actual process exit are observed. Late launch
results are retained and drained too. Closing the last window leaves Terminal
running so another window can be opened. Quit is explicit and drains sessions.

EOF is not a forced process kill. A child that ignores EOF remains owned and can
keep shutdown pending. A cleanup refusal retains its original ticket and cause;
it does not count as released memory. Normal process exit is reported only after
the original account's cleanup requirements are met.

## Remaining work

The shared event-driven SDK executor and resident readiness/completion facilities
will replace Terminal's remaining output/child polling. Completion state must
survive UI queue overflow, wake only interested work, preserve UI thread affinity,
and distinguish cancellation requests from acknowledged cleanup.

Splits using the managed splitter control, themes, a configuration dialog,
selection/copy/paste, richer menus and window arrangements remain subsequent
Terminal work. Preferences UI and the complete environment/profile editor are
not implemented. External Shelly pipelines, general PTY/foreground-job and signal
semantics, and broader process options remain incomplete. Do not advertise those
facilities based on the existing VT parser or a successful echo command.

Kernel, std, boot firmware, extensions and applications must be rebuilt together
when these unreleased ABI contracts change. The development ABI version stays
one; descriptor fingerprints reject incompatible native layouts.
