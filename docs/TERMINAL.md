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

System owns session streams/pseudo-terminal queues and process lifecycle;
Terminal owns presentation. Define stdin/stdout/stderr, terminal detection,
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
routing. The current host Ctrl-to-Command mapping needs a deliberate Terminal
input policy so command interruption and Copy do not conflict; do not globally
change other apps' shortcuts. POSIX signal/job-control compatibility is a later
explicit profile, not implied by implementing an interrupt command.

Drain output and retain exit status after a command finishes. Define Terminal
close/quit, disconnected sessions, blocked writers, broken pipes, cancellation
and child-tree cleanup so no command runs orphaned outside Terminal. Panics use
P8's default System-owned bomb dialog, with session/process context; they do not
replace the reporter with a terminal-only message. App/kernel fault domains
follow [P13](PROCESS-ISOLATION.md).

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
