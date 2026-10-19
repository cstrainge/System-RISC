# Aggregate userspace compatibility tests

All acceptance bodies in this suite import `mac_toolbox::System` and its public
SDK facade. They do not call resident algorithms or private managers. The single
`support::with_system` adapter constructs a System from a live host service table;
`mac_system::testing` then dispatches to the real resident native services.
There are no canned manager results in that harness.

Cursor coverage is in `cursors.rs`, `textedit_cursors.rs` and the ignored
`cursor_graphical.rs` guest test. These check all original System/Finder CURS
and acur records, synthetic indexed color formats, visibility, failure
preservation and actual automatic TextEdit hover. The graphical test launches
the installed Cursor Test app through Finder and compares exact original cursor
sprites and timed frames. See [cursor services](../../../docs/CURSORS.md).

The host harness is enabled only by mac-toolbox's host dev-dependency using
mac-system's `test-support` feature. Its TLS session owns a fresh real Memory
Manager, cleans it up even on unwind, and exposes no manager reference to a test.
Geometry goes to resident geometry dispatch; region/mapping/surface operations go
to the production QuickDraw native boundaries; Memory goes to the same
`memory::native::dispatch` as real ECALL dispatch. Hardware-only Runtime and fonts
are not simulated: unsupported services remain unsupported until backed by a
real fixture/service.

Canonical shared acceptance is `conformance::run_all(&System)`. Its no-std code
owns the assertions and expected counts for20 geometry/region contracts,
267 original oval encodings,3 cleared-handle allocations and48 original handle
observations. Host tests call it through the aggregate API. The RV64 ABI smoke
launcher includes the same module and supplies the real ECALL-backed System;
acceptance logic stays here, rather than being copied into a simulator runner.

Original expectations are frozen under `data/`:

- `original-quickdraw-regions.json` preserves eight basic captured region records
  and267 oval records plus the original report hashes. The `.bin` records are
  verbatim big-endian original region encodings. `original-ovals.bin` is a compact
  21,440-byte corpus for the no-std runner. Host tests compare the JSON and compiled
  fixtures independently.
- `original-memory-cases.json` and `original-memory-actions.json` preserve the
  original System7.6.1 probe's51 observations and setup ordering. The compiled
  oracle includes all48 handle observations. The three NewPtrClear observations
  remain explicitly deferred; this slice exposes owned handles, not raw pointer
  allocations. Invalid original empty-handle size/state outputs are normalized to
  null and tests compare their error codes, never undefined returned values.
- `initial-memory-red-run.json` records the first original-backed public API test
  failing with NOT_SUPPORTED against the real resident stub. The same suite then
  passed after the resident implementation was added.
- `conformance-ledger.json` distinguishes original observations, native safety
  extensions, deferred families and host versus real ECALL verification.

`memory_boundaries.rs` contains separately labelled native API safety tests. Those
bounds/generation/borrow checks are not presented as original Macintosh behavior.
The resident arena's16-byte alignment/12-byte accounting overhead is a bounded
native policy fitting observed guarded growth/append outcomes; neither the tests
nor the oracle establish the original heap's physical allocation layout.

Run the host userspace suite with:

```sh
cargo test --manifest-path System7/Cargo.toml -p mac-toolbox --tests
cargo clippy --manifest-path System7/Cargo.toml -p mac-toolbox --tests -- -D warnings
```

This is the initial aggregate corpus, not a claim that every legacy internal
unit/visual test has already migrated. Public API compatibility for more manager
families should extend this suite and the same shared acceptance entry point.


Window interaction acceptance lives in `window_interactions.rs`. Its ignored
hardware tests run after building the disk image:

```sh
cd System7
cargo xtask image
cd ..
cargo test --manifest-path System7/Cargo.toml -p mac-toolbox --test window_interactions -- --ignored --test-threads=1
```

The capture helper only launches a separate read-only headless
guest, sends real virtio input and returns trace observations/P6 framebuffers;
all new behavioral assertions are Rust tests here. It never attaches to the
visible guest. Finder exercises its normal SDK path. The original-pixel test uses
a standalone application under `fixtures/window-probe`, depending only on the
public aggregate SDK. Its host packaging tool builds a native resource fork and
installs it as Finder on a private cloned image with isolated hfsutils state.
The user's generated disk is hashed before/after and never modified.

`data/original-window-tracking.json` records original image hashes, actual pointer
positions, committed bounds and observed RGB pairs. The two original mask files
retain all changed positions of the 640×480 held drag/grow frames. Rust checks all
614,400 positions plus exact colors where the original control-filled window and
the minimal white probe have identical source pixels. Framebuffer roundtrips and
press cancellation/reentry use complete, unmasked frames. These are bounded held
states and input sequences, not a claim of exhaustive animation-frame sampling.


ABI4's `strings.rs` compares native UTF-8 SDK calls with the separate raw
Pascal/MacRoman traps, through shared resident font/control/resource/surface/file
boundaries. The host HFS fixture tests both string input and output and preserves
2026 metadata. Graphical text/window/log acceptance runs in the real disk-boot
conformance guest; the isolated non-graphical ABI harness does not claim that
coverage. UTF-8 errors, malformed Pascal prefixes, all255-byte expanded titles,
unrepresentable legacy output and undersized output buffers are explicit cases.

`dates.rs` mounts `data/files/dates-2026.hfs` through firmware-style sector reads,
then reads root/folder/file/volume dates through the aggregate SDK. These are
synthetic2026 regression values, not original-OS observations. Shared date value
checks run in `conformance/dates.rs` on host and RV64. Classic overflow fails;
native64-bit Unix seconds continue past2038 and2040.

The graphical conformance profile runs as a private Finder on a cloned boot disk, with the actual ROM, System resource bundle, native ECALL gate and loaded CDEF0/1. `fixtures/conformance` calls `conformance::run_all`, the real HFS date fixture checks, and the shared/graphical Pascal–UTF8 string checks. The capture script only packages, boots and collects serial; `userspace_graphical.rs` and canonical guest Rust own every acceptance decision. The nongraphical ABI-smoke profile calls `run_core` and makes no control/font/resource/window-string claim.


`concurrent_services.rs` adds a native concurrency contract. Eight host OS threads
share one real resident Memory Manager and the production nonwaiting state
holder; every operation enters through the public SDK and native dispatcher.
This checks atomic requests, per-handle data, actual Busy/retry progress and
explicit disposal/reuse, rather than eight independent TLS-isolated managers.
The host-only shared fixture lives in `mac_system::testing::shared`; no mock
manager results are returned. This does not establish an RV64 scheduler or
multi-application context isolation; those gates are in
[THREADING.md](../../../THREADING.md). `Handle::try_dispose(&mut self)` preserves
ownership on temporary contention; general scheduler-aware RAII cleanup remains
an explicit future requirement.

`menus.rs` installs original captured MENU records through the public SDK and
checks geometry, enabled/check state and typed command selection against the
real resident manager and MDEF. Its ignored hardware test installs an SDK-only
probe on a private cloned disk, then captures real click, drag, hover, release,
cancel and keyboard input. Rust checks command routing, disabled items, menu
switching and complete framebuffer restoration. Three original cursor-hidden
File popup crops supply109,908 strict RGB pixel comparisons, including text,
shortcuts, separators, highlight and frame/shadow edges. Their provenance is
`data/original-menu-pixels.json`; no masks or color tolerances are applied.
Another456 pixels check the original enabled rainbow Apple title, using its
bundled System small-icon resource. Selected/disabled Apple pixels remain a
separate capture gate.
The selected File title adds820 pixels, including the top edge, selection
overhang and uninterrupted bottom bar rule.
An additional native safety regression authors a MENU with no cached width or
shortcut. Its longest label must match the full measured public QuickDraw text
raster, including the final pixels; this invariant is separate from the original
Macintosh pixel count.

Run after `cargo xtask image`:

```sh
cargo test --manifest-path System7/Cargo.toml -p mac-toolbox --test menus -- --ignored
```

The Python helper under `System7/scripts` only packages, boots, sends input and
records observations. Apple/Label icon rows, all custom MDEF protocols and
continuous flashing animation remain outside this captured menu subset.


`events.rs` freezes low-level queue observations from original System7.6.1 and
checks both that20-entry OS queue and the separate128-entry native application
queue through the aggregate SDK. The same `conformance/events.rs` contract runs
in the graphical RV64 fixture. Posting, masks, peek/remove order, stopping flush,
overflow, null records and physical-state timestamps have distinct cases.
The ignored input test uses a separate SDK application and private VM to check
physical key/button transitions through actual virtio input and ECALLs.

`scrap.rs` uses the original ordered records and provenance in `data/scrap`.
Shared no-std cases cover18 original observations and17 native buffer/Unicode
contracts;12 paired Pascal trap cases test conversion at the actual boundary.
Additional host tests check byte/record limits, failure preservation, cleared
transport borrows and eight concurrent OS threads sharing one resident clipboard.
These are memory clipboard contracts. Disk Scrap.dat, original counted-MacRoman
A-line adapters and scheduler/application ownership remain separate.


Uncached menu sizing now has eight executed original CalcMenuSize observations
in `data/menu-sizing`. Both host services and the loaded RV64 MDEF run those
cases through the public Menu Manager. The graphical probe also checks a full
Close Window label, its10-pixel gap, and Command-W against public QuickDraw text
rendering. This covers generated Finder menus whose resource width is zero;
cached original popup pixel comparisons remain in the same test.

## Monochrome TextEdit

`textedit.rs` calls the public SDK for356 original layout/edit/hit observations,
20 native Unicode/clipboard contracts and6 paired Pascal conversion contracts.
The same contracts run through the private graphical RV64 conformance Finder.
Additional host tests exercise editor/byte limits and failed edits preserving
both editor and clipboard. Original private TE scrap remains separate from the
native shared clipboard API.

`textedit_graphical.rs` compares seven original128×96 one-bit frames (86,016
pixels) with actual resident drawing: text, multiline selection, caret, scroll,
blink on/off and inactive selection. Python supplies private-disk transport;
Rust owns pixel acceptance and actual Finder keyboard/menu/resize assertions
in a second private VM. Original probes, small fixtures and provenance are
under `fixtures/textedit-original` and `data/textedit`. Styled text, other
font/alignment profiles, hooks and original TEHandle adapters remain open.

## Typing and native navigation

The shared TextEdit suite adds49 native cursor/anchor cases on host and RV64,
covering UTF8, direction reversal, soft wraps, CR boundaries and preferred
vertical column. A host case also moves through document rows beyond signed
QuickDraw coordinates.

`typing.rs` boots two private guests. `fixtures/typing-probe` includes actual
Finder sources, counts public service calls while forwarding the real gate, and
checks that text keyDown/keyUp handling makes no File Manager calls. Its timing
observations use public60-Hz uptime. A second guest runs the actual Finder with
QMP key input and asserts burst delivery and23 physical navigation/selection
sequences. Capture Python transports acknowledgements; Rust checks all expected
values. Timing data is reported without host-dependent pass thresholds.

Set `SYSTEM7_TYPING_WINDOWS=1` for the same assertions against headless Windows
QEMU guests using a private monitor port. The default gate uses Linux QEMU.

TextEdit graphical acceptance also compares eight native caret-on/off pairs
(98,304 pixel positions) after Return, repeated Return, subsequent typing,
Backspace, deleting the CR, an empty document, a wrapped line and resizing.
Each pair must differ at exactly the expected1×16 caret rectangle; the same
test preserves the seven original pixel snapshots.

`panic_graphical` launches the installed Panic Test through Finder in a private
QEMU guest. It checks the exact original DSAT bomb pixels, formatted panic and
source information, a report over 20 KiB, Home/End scrolling, lossless copy into
a separate TextEdit process, and application-only termination with a usable
desktop afterward. Set `SYSTEM7_PANIC_WINDOWS=1` to use private native Windows
QEMU instead of Linux.


Shared handle storage is accepted through the public SDK in
`conformance/ownership.rs` (actual RV64 windows) and `memory_boundaries.rs`.
The window cases repeatedly reuse a slot while rejecting the old ID across
queries, mutation, drawing and disposal, including the legacy Pascal trap.
They also exhaust the24-bit event-safe generation range, verify retirement,
and reject live foreign memory/window IDs in both directions.
Memory acceptance checks all65535 generations without wrap, the unchanged16/16
wire encoding, stale byte access/disposal, and generation preservation on app
cleanup. Owner/access-denied coverage remains in the process conformance suite.
