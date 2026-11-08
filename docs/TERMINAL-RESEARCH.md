# Terminal research for System RISC

2026-11-05. Source and protocol survey. Stage assignments below are proposed.
The review compares mechanisms and interface contracts. It includes no benchmark
runs or claims about their speed on System RISC.

## Reference implementations

| Terminal and snapshot | Observed mechanism | Relevance to System RISC |
| --- | --- | --- |
| Alacritty, upstream master reviewed 2026-11-05 | Tracks left/right damaged columns for each row and marks affected cursor positions. Tracks full invalidation separately. [Grid and damage source](https://raw.githubusercontent.com/alacritty/alacritty/master/alacritty_terminal/src/term/mod.rs) | Preserve localized writes all the way from a cell mutation to QuickDraw. Allow whole-viewport invalidation for genuine screen replacement. |
| Ghostty, upstream main reviewed 2026-11-05 | Builds render state from dirty grid rows, with a separate full-rebuild path. The snapshot handles wide-cell selection boundaries. [Render state source](https://raw.githubusercontent.com/ghostty-org/ghostty/main/src/terminal/render.zig) | A cached render view can isolate the renderer from parser state. Start with a synchronous bounded patch and add a second snapshot only if target profiling warrants its memory cost. |
| foot, 1.21.0 source and upstream-authored foot.ini manual reviewed 2026-11-05 | Software rasterization with Pixman, shared-memory buffers and damage tracking. Buffer reuse can require restoring damage accumulated while a buffer was unavailable. [Terminal source](https://sources.debian.org/src/foot/1.21.0-2/terminal.h) · [Configuration manual](https://manpages.debian.org/testing/foot/foot.ini.5.en.html) | Closest rendering comparison for a CPU framebuffer. Track what a committed buffer actually contains. Carry dirty state across a refused paint. |
| Windows Terminal, upstream main reviewed 2026-11-05 | Renderer has a paint scheduling path. AtlasEngine receives invalidated areas and includes the old cursor region when the cursor goes invisible. [Renderer](https://raw.githubusercontent.com/microsoft/terminal/main/src/renderer/base/renderer.cpp) · [AtlasEngine](https://raw.githubusercontent.com/microsoft/terminal/main/src/renderer/atlas/AtlasEngine.cpp) | Cursor erasure is real damage. Reuse the invalidation principle. The GPU atlas and renderer thread have different costs on a software framebuffer. |
| WezTerm, configuration and feature docs reviewed 2026-11-05 | Offers a frame-rate cap on several backends and uses compositor pacing on Wayland. Provides scrollback, search/copy and image features. [Frame pacing](https://wezterm.org/config/lua/config/max_fps.html) · [Features](https://wezterm.org/features.html) | Separate parser progress from paint cadence. We need a policy for the native event loop even when display pacing is unavailable. |
| kitty, configuration and protocol docs reviewed 2026-11-05 | Separates input and repaint delays. Graphics protocol supports upload, placement, deletion, replies and animation. Keyboard protocol negotiates richer key information. [Configuration](https://sw.kovidgoyal.net/kitty/conf/) · [Graphics](https://sw.kovidgoyal.net/kitty/graphics-protocol/) · [Keyboard](https://sw.kovidgoyal.net/kitty/keyboard-protocol/) | Design protocol state and image lifetime independently of the graphics backend. Ordinary input should remain responsive during bulk output. |
| iTerm2, protocol and restoration docs reviewed 2026-11-05 | Defines inline images and proprietary control sequences, and distinguishes UI restoration from session restoration. [Images](https://iterm2.com/documentation-images.html) · [Escape codes](https://iterm2.com/documentation-escape-codes.html) · [Restoration](https://iterm2.com/documentation-restoration.html) | Persist window geometry and preferences first. Inline images need explicit placement and clipboard rules even on a CPU renderer. |
| Contour, VT documentation reviewed 2026-11-05 | Documents synchronized output with DEC private mode 2026, so application updates can be presented together. [Synchronized output](https://contour-terminal.org/vt-extensions/synchronized-output/) | Use this as the protocol model for coalesced presentation. Bound the wait so an interrupted application cannot hold the display indefinitely. |

Upstream branch links are rolling snapshots. The dates describe the source
review. The foot release number identifies the source consulted for its layout.

## Stage 4 candidate — native text sessions

- **Damage.** Track changed cell spans at mutation time and accumulate exact
  QuickDraw paint regions. Start with per-row spans, merging adjacent spans only
  where inexpensive. Include old and new cursor/selection footprints. A resize,
  alternate screen switch, exposure, or font change can require a larger
  repaint. Ordinary input with no visual change should generate none. Keep the
  CPU/QuickDraw renderer usable on framebuffer targets. VirtIO GPU can consume
  the resulting damage through the existing display path.
- **Pacing and floods.** Consume a bounded slice of ready pipe bytes per event
  turn. Continue parsing all accepted bytes in order while coalescing paint
  requests. A full pipe provides backpressure to a producer. Keep keyboard,
  close, cancellation and other sessions responsive. Allow ready keystrokes to
  make progress without waiting for bulk output to finish. Measure input-to-echo
  and presentation time separately on the guest. Neither the upstream pacing
  knobs nor source inspection give us comparable latency numbers.
- **Grid, width and scrollback.** Keep one active viewport and bounded history
  per session. Store row boundaries, soft-wrap information and wide-cell
  ownership explicitly. Evict complete old history rows under a byte budget.
  Avoid copying the full grid to advance scrollback. Search and selection must
  respect wrapped lines, combining characters and wide glyph continuations.
  Preserve enough structure for search even if its UI lands later.
- **Fonts and paint.** Start with a measured fixed-width font through the OS
  font service. Cache stable metrics and glyph results under a budget. A wider
  glyph or cluster occupies an explicit number of cells. Later fallback and
  shaping must keep cursor position, selection and damage consistent when
  ligatures, emoji and styled spans cross cell boundaries. Avoid shaping the
  full viewport on each update.
- **Core text controls.** Define CR/LF, backspace, erase, tab stops, wrap at the
  right margin, SGR attributes and basic cursor motion against the model.
  Test fragmented UTF-8 and escape input through real pipes. Keep the cell
  model ready for indexed and true colour, alternate screen and scrolling
  regions, while limiting advertised capabilities to the implemented profile.
  Compare terminal behavior with the published VT sequences.
  [Microsoft VT reference](https://learn.microsoft.com/en-us/windows/console/console-virtual-terminal-sequences)
- **Input and sessions.** Route keyboard input independently of the display.
  Keep native command shortcuts separate from CLAP input; preserve a distinct
  action for Copy and foreground interruption. Agree on cooked/raw ownership
  before defining the ABI. Recommended service semantics are cooked editing
  and echo when requested, raw input for full-screen applications, and a
  session-scoped interrupt rather than an ambient signal. Represent resize as
  a checked session event. Keep stdin EOF distinct from interruption, and
  distinguish Backspace from Delete and cursor/application key modes.

## Stage 5 and later candidates

| Mechanism | Proposal and System RISC cost |
| --- | --- |
| Full VT state | Add scroll margins, origin mode, alternate-screen variants, DEC special graphics, save/restore, tab handling and wrap-pending behavior to the supported profile. Specify query/reply behavior and verify every advertised mode. Avoid importing Unix job-control semantics through escape handling. |
| Colour and styling | Complete indexed and 24-bit palettes, bold/italic, strikeout, coloured underline, undercurl and blink. Use effective changed ranges after palette or style changes. Blink invalidates the affected spans only. |
| Paste, focus, mouse and titles | Add bracketed paste, focus reporting, SGR mouse, OSC 8 links, and OSC 0/1/2 titles. These require session-local state and bounded parser buffers. OSC 52 is a separate clipboard authority decision. Native Paste and Copy use the existing clipboard service. |
| Extended keyboard | Implement legacy arrows/function keys and modifiers first. Add negotiated kitty keyboard and, where needed, modifyOtherKeys without conflating character input, physical key events or application commands. Keep TTY Control separate from GUI Command and qualify the mapping on native hardware before shipping interruption. Keep XON/XOFF and Unix-style suspend outside the first native profile pending a compatibility requirement. |
| Synchronized output | Support DEC 2026 as an explicit presentation policy with a deadline and bounded resources. Continue parsing and keep cursor/input handling responsive while a synchronized batch is open. |
| Graphics | Implement sixel and full kitty graphics including animation as staged protocol work. Preserve image identity, placements, crop, z-order, scrollback association and deletion separately from glyphs. Track only changed placement pixels. Bound compressed input, decoded pixels, frames and references per session. A visual clipboard copy needs an explicit policy for image plus text. Defer iTerm2 image escape compatibility until there is a concrete consumer. |
| Search, selection and copy | Support linear selection over wrapped lines and wide clusters. Add history search without making the paint loop scan history. Define extraction rules for text, links and pictures before shipping graphical selection. |
| Persistence | Save versioned JSON preferences and window geometry first. Review session documents and history persistence separately. They require storage, recovery, privacy and process-lifetime policy beyond restoring a window. |
| Capability advertising | Define TERM, queries and any capability table from the implemented profile. Tie changes to actual escape and keyboard coverage. Keep unsupported facilities out of the advertised profile. |

## Delivery gates

1. **Pipes.** Anonymous pair creation exists in the public SDK. Exercise
   sustained native reader/writer/close overlap and blocked-call cancellation
   on exit before relying on pipes for a process launch.
2. **Execute and attachment.** Establish one checked launch request, including
   argv, environment, working folder and stream ends. Atomically validate and
   transfer the ends when the child becomes runnable. Failed launch preserves
   Terminal's ownership. Bind child stdio before CLAP entry. Keep GUI null
   streams on their existing path. Route CLAP handling through the registered
   Cli Extension and preserve a typed refusal when no handler claims it.
3. **First Terminal.** One session per window. Demonstrate an actual CLAP
   pipeline, independent stderr, EOF, exit status, cancellation, failure
   recovery and a second session. Confirm damage-only paint and input-to-echo
   latency with output under load in the native guest. Start with a small CLAP
   fixture, then port Shelly against the same launch and stream contracts.
4. **Full Terminal.** Review protocol scope and budgets, then qualify each
   supported capability on the production runtime. Keep VM framebuffer evidence
   separate from hardware timing evidence.

Installation follows the [Terminal spec](TERMINAL.md#installation). The native
shell port and the graphics scope can advance independently once the launch
contract is in place.

## TTY compatibility target

Linux and BSD provide paired PTY endpoints. The emulator owns the host
endpoint. The child gets a terminal endpoint whose line discipline supplies
canonical editing, echo, control-character interpretation and job-control
behavior. A shell or full-screen program can select noncanonical input and
perform its own editing. The preview's Ctrl+D closes its input pipe. A TTY's
canonical VEOF delivers buffered bytes, or a zero-length read on an empty
line, while the terminal stays open.
[Linux N_TTY](https://docs.kernel.org/driver-api/tty/n_tty.html)
[OpenBSD PTY](https://man.openbsd.org/pty.4)
[OpenBSD termios](https://man.openbsd.org/termios.4)

Windows ConPTY gives the child a console session and carries input and output
over separate channels to the host. Microsoft documents servicing the
channels independently to avoid blocking one direction behind the other.
System RISC can keep its existing authenticated launch contract and expose a
session endpoint for interactive CLAPs while continuing to support ordinary
pipes for redirection and capture.
[Microsoft ConPTY](https://learn.microsoft.com/en-us/windows/console/creating-a-pseudoconsole-session)

For the display, xterm documents cursor movement, visibility and shape as
terminal state. The renderer should own a cell grid, including cursor position
and wide/combining characters, and update changed cells through QuickDraw.
Start with a documented VT/xterm profile and advertise its TERM capabilities
only after qualifying them. The current line preview draws a steady caret
after the prompt and input. It has no application-controlled cursor modes.
[XTerm control sequences](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html)

Alacritty separates the shell executable and arguments from cursor appearance.
It supports a steady beam, underline or block and an unfocused cursor style.
WezTerm defaults to an interoperable terminfo profile and offers a separate
definition for its additional capabilities. Use separate settings for the
startup shell, visual cursor preferences and application-visible capabilities.
[Alacritty configuration](https://alacritty.org/config-alacritty.html)
[WezTerm TERM](https://wezterm.org/config/lua/config/term.html)

Qualify the child-facing session with terminal detection, canonical and raw
input, echo disabled for secrets, EOF, interrupt and foreground job control,
window size and resize delivery, and hangup. Qualify the renderer separately
with cursor addressing, erase and insert, scrolling regions, alternate screen,
colors, Unicode cell width, key encoding and bracketed paste. Exercise both
together with the Shelly port and a full-screen program. Include two sessions,
output backpressure, closing one session, and repeated reuse.

### Engine and renderer boundary

The session engine owns the attached process, input discipline, output ordering
and bounded queues. It encodes keys according to the current terminal modes and
turns the output stream into ordered display operations. The renderer owns its
own cell grid, scrollback, cursor geometry and damage. Its painting code uses
that retained view without reading pipes or editing process buffers. Mode and
query responses go back through the session engine. This separation lets a
window resize or expose repaint from retained cells while input and output
continue independently.

### Launch and exit responsiveness

The first interactive image was sampled with three ordinary Hello Probe
launches through the native picker in a private 2 GiB Linux QEMU guest.
Host serial observation gave 571–1174 ms from picker acceptance to
PROCESS_LAUNCHED and 1459–2322 ms from PROCESS_STOPPED to
TERMINAL_CHILD_EXIT. These are wall-clock samples with other local workloads,
serial output and host polling present. They are not hardware timings.

Process Manager publishes the parent result when the child stops. An initial
owner cleanup walk follows before the invoking path returns to scheduling,
and actual thread and account reclamation continue through the existing
retirement machinery. Profile launch admission and image/thread setup
separately. On exit, profile the initial walk and the time until the parent
can run. Preserve the independent reclamation barrier when changing which
work runs before the first scheduling turn.

### Shelly on System RISC

System already admits a CLAP with its startup directory bound to the user's
Documents directory. The System RISC std backend can query that directory and
change directories, while its generic home-directory query currently returns
no path. The Shelly port should capture its initial Documents path as the
shell's home, then use that value for tilde expansion and bare cd even after
the working directory changes. Keep command lookup in Applications/Utility/Cli.

Shelly's host process layer currently selects Unix process and terminal
implementations, with an unsupported fallback elsewhere. Port the interpreter
and a System RISC process adapter against the native launch contract. Enable
only the features that have a working platform implementation. Leave network
clients and servers and other unsupported facilities outside the initial guest
build.
