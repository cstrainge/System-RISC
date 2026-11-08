# Terminal and CLAP applications

Status: design/backlog, tracked as [P11](CORE-TOOLBOX-ROADMAP.md#p11--terminal-and-clap-applications).
Introduce `CLAP` as the Macintosh file type for command-line applications, with
a resource-based Terminal application hosting their sessions. This is not yet a
new manifest option, process service or installed application.

## Application and session contract

- Terminal is an ordinary std-based APPL with its own OS thread, resource menus,
  icon, windows and JSON preferences. Each CLAP is a separate std process with
  its own AppId, main thread, memory, environment and exit status; it does not
  execute on Terminal's UI thread. Native workers remain within their process.
- CLAP packages retain normal native application code/descriptors/relocations
  and Cargo-derived versions. CLAP is a file/launch class, not a new instruction
  format or code-resource type. Extend mac-build/xtask and FORMAT.md when the
  exact launch metadata and startup ABI are implemented.
- A CLAP must have a live, kernel-validated Terminal session before entry.
  Finder opening/double-clicking CLAP routes to Terminal to create a session and
  launch the command. Direct launch without a valid session is rejected; merely
  supplying streams, an environment variable or a claimed Terminal name is not
  sufficient. Only the registered Terminal host creates sessions; children may
  inherit a session through checked process creation.
- Each command invocation creates a new process, even for the same file. Keep
  APPL's existing activate-running-file behavior separate. Runtime running-file
  state still uses actual volume/CNID identity. Terminal owns session UI and app
  switching; commands do not each create a separate GUI menu bar/window. A CLAP
  may request launch of a separate APPL through normal services.
- Terminal and Process Manager expose checked session/child identities through
  the shared generation-tagged facility, with separate ownership/delegation.
  Session identity is out-of-band authority, not an environment string or PID.
  Document UI-service restrictions for the terminal-only launch class.

## Installation

- Install the Terminal application in `Applications:Utility`.
- Install all command-line applications, including Shelly, in
  `Applications:Utility:Cli`. This directory is the first entry in the
  default command search path.

## Environment preferences and inheritance

Terminal's versioned JSON preferences own a `base_environment` string map and
the default command/search-path settings. Provide a preferences UI to edit them;
do not borrow the host computer's environment or treat Finder's environment as
the defaults. Validate keys/values, limits and native path-list syntax together
with P8, rejecting unsupported encodings and embedded NULs.

When starting a session, snapshot the configured base map, then apply explicit
session and top-level launch overrides. Child CLAPs inherit their parent's
effective snapshot plus explicit per-child changes. Support std Command env,
env_remove and env_clear semantics; clearing environment does not discard the
out-of-band Terminal attachment. Changes made by a child or later preferences
edits must not mutate running siblings/parents. Preference changes affect future
sessions unless an explicit session update is requested.

Derive terminal capability values from the emulation actually implemented.
Define precedence and validation for TERM and session-provided values rather than
advertising unsupported behavior. Working directory and argv are separate launch
fields, with owned snapshots and fallible argument parsing; they are not inferred
from arbitrary environment entries. Command search follows the defined native
PATH syntax and explicit path rules.

## Streams, terminal behavior and lifecycle

System owns session admission, endpoint lifetimes and process lifecycle;
Terminal owns presentation. Full pipe functionality is planned in a pipes
extension replacing the kernel's current placeholders. The std platform must bind
the normal service/module boundary so that implementation can replace the stubs,
without growing a parallel full pipe stack in the resident kernel. Define stdin/stdout/stderr, terminal detection,
partial reads/writes, EOF, backpressure, resize notifications and readiness.
Redirection and pipelines keep the same Terminal session even when a particular
stream is a file/pipe rather than the display. A GUI caller requesting a CLAP
must route through Terminal or receive a defined session-required error.

Start with a documented UTF-8/VT-style emulation profile, then expand tested
escape/control support. Use resident fonts, monospace cell metrics and explicit
emoji/wide-cluster rules; render and select graphemes without corrupting original
text. Include keyboard input, line editing/paste, selection/copy, configurable
bounded scrollback and CDEF scroll controls. Separate terminal emulation from
command parsing; a command runner or future shell is itself a CLAP.

Specify cooked/raw input, echo, interrupt/end-of-input actions and foreground job
routing. In the QEMU host layout, left Alt supplies Command, Ctrl supplies
Control, and right Alt supplies Option. This keeps TTY control keys distinct
from application menu shortcuts. POSIX signal and job-control compatibility
requires a later explicit profile.

Drain output and retain exit status after a command finishes. Define Terminal
close/quit, disconnected sessions, blocked writers, broken pipes, cancellation
and child-tree cleanup so no command runs orphaned outside Terminal. Panics use
P8's default System-owned bomb dialog, with session/process context; they do not
replace the reporter with a terminal-only message. App/kernel fault domains
follow [P13](PROCESS-ISOLATION.md).

## Execute integration

Execution goes through one ordered chain that can accept multiple registered
extension handlers. Extensions are tried in reverse dependency order. Each
handler defines its own claim criteria using broker-validated catalog metadata
and bounded, read-only inspection of the executable. For example, Cli Extension
checks for catalog type `CLAP`, while a 68K emulator could check for type
`APPL` with a `CODE` resource. A handler that passes leaves the request
available to the next handler. An error after claiming a request is returned to
the caller. System's default native executable handler always gets the last
attempt if all registered extensions pass.

The current Toolbox launch call carries the executable's volume and catalog ID.
Its resident launch path reserves an application, prepares heap, TLS and address
space, pins the executable, then creates the thread and activates the app. The
console path must use the same admission and retirement machinery while leaving
Terminal as the foreground UI. The image's kind and the authorized session
determine the launch profile.

- The public execute request names the program, argv, environment, working
  folder and typed child stream ends. The broker copies request data into
  owned storage before asynchronous launch work and checks the session and
  sender's rights to the executable and each endpoint.
- Terminal supplies the stdin reader, stdout writer and stderr writer. The
  pipe manager must preflight all three as a single set, including distinct
  handles, directions and live ownership. Prepare child-owned handles before
  publishing any transfer. Preserve the pipe allocation receipt and endpoint
  counts across the ownership change, including on failed launch.
- Publish the child's stream record before making its thread runnable. The
  commit consumes the offered ends from Terminal and leaves its retained ends
  usable. A failed launch releases prepared child resources and leaves the
  offered ends with Terminal. There is no general process-to-process handle
  transfer API.
- A child obtains its own startup attachments from System. The std backend
  binds separate stdin, stdout and stderr implementations to those attachments.
  The existing GUI launch profile continues to supply null streams.
- Register Cli Extension as the CLAP execute participant in the first native
  launch milestone. Claim only files whose catalog file type is `CLAP`.
  Pass every other type through unchanged. On a claim, direct System to attach the
  request's three specific pipe ends instead of the base handler's stub
  streams. The broker owns handler ordering, session authority and endpoint
  publication. Terminal calls the ordinary process API. Reject an unclaimed
  CLAP or a direct CLAP launch without a live authorized session with a typed
  error.
- Keep headless child launch, wait and retirement independent of Finder's
  foreground activation. Only GUI apps appear in the Application menu or
  receive their own menu bar. Terminal stays the visible UI for its CLAP
  children. Exercise concurrent instances of one CLAP, process exit while
  blocked, and failure before and after launch preparation.

The initial EXEC call carries only the target's volume and catalog ID. Each
participant requests its own evidence. Cli Extension asks the broker for the
catalog type and creator before deciding whether to claim. The broker scopes
the query to the original file and runs it under the requesting process's
file access. It does not load the native resource fork before dispatching the
chain. A handler can request bounded positional reads of either fork on a
native block-backed volume. Each read is limited to 4096 bytes and each
handler may make up to eight queries per dispatch. A short read marks EOF.
Provider-backed fork probes currently return `NotSupported`.

Cli Extension selects the three offered endpoints after claiming a CLAP.
The broker checks the selection against the caller's offer. The Process Manager
publishes child-owned handles before the suspended thread can run. The child's
std startup reads the authenticated attachment record. The Toolbox exposes
this first launch path as `Pipes::launch_cli`. The QEMU RISC-V platform includes
Cli Extension. Activation of an existing GUI instance is still a separate
fast path. System records the installed Terminal's catalog identity at boot.
Streamed launches require a live window owned by that Terminal instance. CLAP
children inherit the session and may launch another CLAP while the window and
host are still live. System rechecks the session before publishing child pipes.

Terminal is installed in Applications/Utility. Opening it directly starts Shell.
File > New Shell creates another shell session. File > Open Command starts its
picker in Applications/Utility/Cli when present. Finder delivers a double-clicked
CLAP through the existing document-open service, and Terminal opens it in a
separate window. A process started to receive documents waits for delivery
before opening a window, so a cold CLAP open does not launch a shell first.

Each window has its own child and three pipes. The retained grid fills the
content area beside a managed vertical scrollbar. The primary screen holds up
to 2,000 additional lines. The alternate screen has its own grid and no
scrollback. The cursor is a blinking block measured from a space in the
terminal font. Terminal understands cursor movement, erase and scroll
operations, foreground and background SGR colors, and bounded RGB sixel
rasters with declared square-pixel geometry. The sixel preview retains one
raster per screen and composites its background as opaque.

In the primary screen, Backspace edits the current line, Enter submits it,
and Ctrl+D closes stdin after queued bytes are sent. Alternate-screen input
is forwarded immediately, including arrow sequences and control characters.
The System RISC Crossterm backend can request immediate input on the primary
screen and query cursor position and screen dimensions over the session pipes.
Terminal's default shell in the QEMU image is the Shelly CLAP. It looks for
`.shelly_init.shy` in the startup volume's `System Folder/Preferences`.
The guest build excludes networking, Unix process replacement and external
pipeline stages. General TTY mode APIs and Unicode cell width remain open.

The first native `std::process::Command` path resolves a program by native
path and launches it through the CLAP handler. Use piped stdout and stderr,
and piped or null stdin. `Command::output` closes the input end and reads both
outputs concurrently. A parent can block on the child's result or poll it.
Dropping the child object releases result retention. Argument and environment
overrides, inherited or redirected stdio, and kill are not yet available.

## Delivery and acceptance

1. Extend packaging/launch classification, session authority and normal-main
   startup. Add Terminal plus simple echo/env/exit CLAP fixtures using P8 std.
2. Deliver session queues, terminal input/output, resize/scrollback and JSON
   environment preferences; prove parent/child inheritance and isolation.
3. Add command runner, redirection/pipes and Command integration with readiness,
   cancellation and complete process status. Then qualify interactive tools.

Canonical public service checks belong in `crates/mac-toolbox/tests`; std guests
also exercise env/args/streams/Command through P8. Private graphical tests cover
Finder-to-Terminal launch, repeated invocation, two sessions/apps, output floods,
Unicode/escape boundaries, EOF/exit/panic, stale/foreign sessions, rejected direct
launch and Terminal shutdown. Verify defaults persist across reboot and changes
affect only intended sessions. Bounded queues, row/cell damage and readiness
wakes must preserve Cursor Test and input/present timings under sustained output.

This is the command application foundation for [hosted Rust tools](HOSTED-TOOLCHAIN.md),
not a claim that a shell, Unix process model or compiler already runs.
