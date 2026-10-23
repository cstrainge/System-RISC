# Finder selection, performance, and Drag Manager — spec

For the coding agent. Three workstreams, one machine, in this order:
(1) the selection performance fix, (2) multiple selection, (3) drag and
drop through a new system Drag Manager with the Finder as first client.

## 1. Why one spec

Selection is a **set of items**. The performance bug exists because the
Finder treats a selection change as "recompute everything"; the fix
treats it as "diff the set, repaint the diff." Multiple selection is
that same set with more than one element. A drag's payload is the set,
handed to the Drag Manager. Building them separately would mean
building the set machinery three times. Build it once.

House rules apply throughout: the AGENTS.md performance bar; Chloe's
MHz budget ("build as if we were on a 68k machine in the MHz range" —
work per interaction must be proportional to the *change*, never to
the whole state); every object ID through the shared generation-tagged
facility; typed errors; RAII owners in the public SDK; canonical tests
in `crates/mac-toolbox/tests` via the public SDK.

The Finder source lives in `apps/finder/`, alongside the other applications.
Its packaged file remains physically installed in the blessed **System Folder**.
Cargo, builders, fixtures and reference tools use that single source tree.

## 2. Part 1 — the performance fix (substrate)

Finding, for reference: `Browser::click` hit-tests by re-enumerating
the folder (a `Files::entry` trap plus a `text_width` trap per item,
rebuilding layout from scratch), then calls `redraw_all()` — desktop
plus every window, each redraw re-enumerating and re-plotting. One
click ≈ a hundred trap round-trips; the app misses its event-poll
window and the system shows the watch. (Full audit in the
infrastructure review log, 2026-10-20; `317d030` already queued a
task doc, FINDER-SELECTION-PERFORMANCE.md — this spec is its
authoritative form.)

Rulings (Chloe, 2026-10-20):

1. **Cache item information per folder in the Finder** — entry info,
   icon, and text dimensions — so hit-testing and repaints work from
   the cache and text is never re-measured per click or per draw.
   **Any filesystem change to that folder clears that folder's
   cache.** Key/validate the cache off the files service's catalog
   revision machinery (`system/src/toolbox/files/scan.rs`; the
   `ScanStale` typed error already exists for exactly this).
   Subscribe to [resident file notifications](FILE-NOTIFICATIONS.md) while the
   folder is open, including background windows; coalesce changes rather than
   polling or enumerating on clicks. Read-only mounts permit quiet watches.
2. **Repaint through a mask.** When selection changes, repaint with
   the mask/clip set to exactly the items involved in the change —
   the bounds (icon + label) of the items that left the set and the
   items that entered it. The draw routine itself stays uniform;
   only the mask changes.
3. **Early-out on no change.** A click that doesn't change the
   selection does no enumeration, no repaint, no work.

Acceptance for Part 1:

- Changing selection in a browser window performs **zero** folder
  enumerations (hit-test and repaint both served from the cache).
- A selection repaint touches only the masked items' bounds.
- No watch cursor on selection change, in emulation, with several
  windows open.
- The cache survives until its folder changes on disk; an FS write
  to the folder (create/rename/delete/move within it) is visible on
  the next draw without a manual refresh.

## 3. Part 2 — multiple selection

### 3.1 Model

- Per-window selection becomes a **set** of items
  (`Location` = volume + cnid; the desktop keeps its own set).
  Replace `selected: Option<Location>` and friends; migrate
  `selection()` / `info_selection()` and all consumers
  (`open_selection`, menu enablement) to set semantics.
- There is exactly **one mutation path**: compute the new set, diff
  against the old, repaint through the mask of the symmetric
  difference (Part 1, ruling 2). Every gesture below is expressed
  as a set operation feeding that path.

### 3.2 Gestures (icon view)

- **Click an item**: selection = {item}. Click empty space:
  selection = {}.
- **Shift-click an item**: toggle that item in the set (classic
  Finder icon-view behavior).
- **Rubber-band (marquee)**: press on empty space and drag. The
  band selects every item whose bounds **intersect** it. Live
  feedback on each mouse-move: hit-test is pure rect arithmetic
  over the cached geometry (zero traps), and the repaint mask is
  the symmetric difference between the previous and current band
  sets, plus the old and new band outlines. **Shift-marquee**
  unions the band set with the existing selection instead of
  replacing it.
- **Select All** (Cmd-A): selection = all visible items in the
  front window.
- Dragging is Part 3, with one selection rule that belongs here:
  **pressing an unselected item selects it alone** (set = {item})
  before the drag begins; pressing a selected item keeps the set.

### 3.3 Acceptance for Part 2

- All gestures mutate selection only through the single diff/mask
  path (no gesture has its own repaint code).
- Marquee drag at emulation speed: no enumeration per mouse-move,
  repaint mask per move is bounded by the items the band edge
  crossed, no watch.
- Menu enablement and Open/Get Info act on the set (see DECIDE 3
  for Get Info).

## 4. Part 3 — the Drag Manager

Drag and drop becomes a **system manager**, not Finder-private code:
new SDK crate `toolbox/drag` (name indicative) over a resident
service, following the classic Drag Manager's design (System 7.5;
API target per the project's founding rule is the most advanced
classic-series form). The Finder's existing private drag tracking
(`Browser::drag`, the hover/threshold logic in `track()`) is
retired in favor of the manager.

### 4.1 Session model and ownership

- A drag is a **session object** created by the sender:
  `Drag` owner in the SDK (RAII: dropping the owner before tracking
  cancels the session), its ID issued and validated through the
  shared generation-tagged facility like every other object.
- Sender API shape: create a drag → add items (each item is an
  opaque sender-chosen index) → offer flavors per item → `track()`
  → outcome (`Dropped` to a receiver / `Cancelled`).
- `track()` runs the gesture: the manager owns pointer tracking,
  feedback, and receiver negotiation until drop or cancel, then
  returns. **The tracking loop must yield/poll so no participant
  trips the responsiveness watchdog**, and feedback work per
  mouse-move is bounded (old outline ∪ new outline ∪ changed
  highlight only).

### 4.2 Flavors and promises (the classic design's best idea)

- Data is offered as **flavors** keyed by `TypeCode`, per item —
  the drag analogue of clipboard types.
- Two kinds: **inline** data (small payloads, supplied up front)
  and **promise** flavors: the sender registers that it *can*
  produce the data; the manager calls the sender's promise keeper
  **only if a drop is accepted and the receiver requests that
  flavor**. Promised data is produced at most once, never
  speculatively. Dragging must cost an outline and metadata, not
  a copy.
- Receivers inspect the offered flavor list (types and item count)
  *without* pulling data, to decide highlighting; they pull data
  only inside the drop.

### 4.3 Receivers

- An app registers drag interest **per window** with the manager.
- During tracking, the manager determines the hot window and
  notifies the owning app through the event stream — new variants
  on the decoded `Event` enum (it is `#[non_exhaustive]` for this):
  drag-entered (with item count + offered flavor types), drag-within
  (position), drag-left, and the drop itself. The receiver draws
  its own highlight in response (the manager does not guess what a
  valid target looks like); the standard folder highlight is a
  Finder behavior, not a manager feature.
- A drop delivers the drag session ID to the receiver, which pulls
  the flavors it wants via manager calls, then acknowledges
  (consumed / rejected). Rejected or missed drops get the classic
  **zoom-back** animation.

### 4.4 Feedback

- V1 feedback is the classic **outline** drag image: the outline
  (or bounding region union) of the dragged items' cached bounds,
  drawn by the manager. Translucent image dragging (Mac OS 8.5)
  is a later addition — see §7.
- All drag feedback obeys the mask discipline from Part 1:
  damage per move = old feedback ∪ new feedback ∪ any highlight
  change. No full-window repaints during a drag, ever.

### 4.5 Finder as first client

- **Sender**: Finder icon drags offer one item per selected file,
  with a **file-reference flavor** (the list of (volume, cnid))
  plus promise flavors for textual representations (name/path) if
  a receiver asks. The payload is references, not file bytes —
  classic Finder drags moved references, and so do these.
- **Receivers**: folder windows, the desktop, volume icons, and
  the Trash. Drop semantics:
  - Same volume: **move**.
  - Different volume: **copy**.
  - **Option** held at drop: force copy (even same volume).
  - **Command** held at drop: force move (even across volumes). On the current
    native keyboard mapping, the host Ctrl key supplies Command.
  - Sample modifiers at **drop time**, never latch them at drag start. The user
    can change their mind while dragging. If both forcing modifiers are held,
    Command takes precedence; Trash takes precedence over both.
  - On the Trash: always move to Trash, regardless of modifiers. A read-only
    source on **any read-only mounted filesystem** cannot be trashed or moved;
    enforce this through the File Manager's filesystem capabilities, not an
    archive-type special case. Report that error rather than
    silently copying it or deleting a partial source.
    A read-only **destination** rejects both incoming copies and moves. Finder
    must reject/highlight it as invalid during negotiation; the File Manager
    revalidates writability at execution before staging or changing any source.
    This applies to every mounted filesystem, not just `.sit` archives.
  - **Escape cancels the entire operation**, not just one item. Before drop,
    cancel negotiation, zoom back and change no files. During a transfer, stop
    scheduling new work and roll back the operation's own staged outputs and
    reversible moves. Preserve original files until copy/verification succeeds;
    do not treat partial batch completion as successful cancellation. Report
    rollback or cleanup failures honestly. Power-loss atomicity is a separate
    filesystem guarantee, not implied by Escape cancellation.
  - Recursive folder transfers preserve both forks and Macintosh metadata.
    Same-volume moves retain file identity; copies receive fresh destination
    identities. Cross-volume moves copy/verify before removing the source.
    Reject self/descendant destinations and resolve name collisions explicitly;
    never silently overwrite an unrelated item.
  - The receiver performs the operation through ordinary Files
    calls **after** the drop; the manager moves no data itself.

### 4.6 Persistent icon placement

Icon positions are per-entry Finder metadata attached to `(volume, cnid)`,
never an index in an enumeration or a map keyed only by the displayed path.
The existing catalog Finder-info fields are the authoritative saved position.
Folder layout uses those positions when initialized; automatic layout is only
for unplaced items. Persist changes through resident Files services, with the
same support on HFS-B and writable provider volumes (host sidecars). Read-only
volumes expose their saved positions without attempting writes.

- A same-volume move retains identity and all Finder metadata. A drop supplies
  the new position in the destination folder's content coordinates, including
  scroll offset, preserving relative offsets within a multiple-item group.
- Copies get fresh identities and independent position state; destination
  placement changes must never alter the source's saved position. Cross-volume
  copies/moves preserve both forks and other Macintosh metadata while assigning
  the destination's drop positions. Descendant folder layouts remain intact.
- Coordinate conversion belongs to one Finder placement path. Updating a
  position changes only placement fields/initialization; it must preserve file
  type, creator, labels, folder-window state, file dates and unrelated Finder flags.
- Commit on accepted drop, never on mouse motion. Cancel/failed batch restores
  previous placement with the operation's metadata rollback. Persistence errors
  are reported; don't pretend an in-memory position was saved.
- Acceptance: reposition, close/reopen and reboot; rename/move while retaining
  identity; same/cross-volume copy; writable host sidecars; independent copies;
  scroll-coordinate conversion and multiple-item offsets; cancellation and
  read-only rejection preserve every source's metadata.

Long filenames wrap at a bounded caption width (currently up to 96px of Geneva
9 ink in folder windows, or 60px in the desktop's 64px volume cells, plus selection
inset). Cache line breaks and measured widths once per folder
snapshot, preserving the entire UTF-8 name, preferring word/grapheme boundaries.
Automatic rows reserve the full caption height. Saved positions stay authoritative.
Hit tests, marquee intersections, selection masks and scroll extents include
all caption lines; neither wrapping nor measurement happens on pointer paths.

### 4.7 Finder transfer window

Implementation batch (unverified): the resource-based `copy_window` controller
now uses the original Finder7.6.1 fvew8751 geometry and STR#8750 messages,
projected into cpUI8751. It owns managed Static Text/Progress Bar/Stop controls;
Stop/Escape latches whole-operation cancellation and remains visible until the
transfer owner acknowledges cleanup. The unverified same-volume group-move
path now owns it across normal Finder events; recursive copy integration is
still open. Automatic position saves also advance through stepped groups.
The original labels, bounds and font policy are recorded in
`provenance/finder-copy-window.json`; the renderer now follows the traced original CODE174 frame, colour and
monochrome paths; live interaction/pixel parity remains deferred. New definitions are
`definitions/progress-bar` (`CDEF_PROGRESS_BAR`, resource -32760, “Progress Bar”)
and `definitions/static-text` (`CDEF_STATIC_TEXT`, resource -32759, “Static Text”).
The System boot panel now uses the same managed Progress Bar and loaded CDEF;
the old bootstrap-only bar implementation has been removed.

- File/folder copies and cross-volume moves show a resource-based Finder copy
  window using the classic **managed progress-bar control** and **managed static
  text controls**, with the original managed Stop button. If a control is missing, add
  its CDEF and resident/public Control Manager support; no Finder-local imitation.
- Show the operation, current item and meaningful completed/total progress.
  Discover totals with bounded background work; do not block input or invent a
  completion percentage while the total is unknown. Include both forks in byte
  accounting, handle zero-length files and directories, and check overflow.
- Cancel and Escape request cancellation of the entire transfer batch under
  §4.5, including its own cleanup/rollback. The window stays responsive while
  work drains and reports failures honestly; a cancellation request alone is not
  proof the operation has been undone.
- Repaint only the changed progress fill and changed text bounds, early-out on
  identical values, and coalesce progress to visible changes. No folder scans,
  full-window repaints or event storm per copied chunk. Close after successful
  completion/cancellation and release all managed controls/windows through RAII.
- Once this progress-bar CDEF/control is available, replace the kernel boot
  screen's custom progress drawing with the same managed control. Establish its
  resources/services early enough for boot, before Finder or any application
  event loop exists. Preserve desktop-background styling and bounded updates;
  do not keep a duplicate kernel-only progress implementation. The ROM's early
  Happy Mac/missing-disk indicators remain a separate bootstrap concern.
- TextEdit is the validation second client when its surface is
  ready (text flavor, promise-kept) — not a Part 3 deliverable.

### 4.8 Legacy surface

The classic Drag Manager trap surface (for legacy apps) is
**not** part of this spec; it belongs to the compatibility-closure
work. The native design must not preclude it: classic handlers are
callbacks, native receivers are events — the manager's core session
state is shared, and the event surface is the native projection of
the same negotiation. Note the mapping in the manager's docs.

## 5. Performance rules (apply to all three parts)

- No folder enumeration on any pointer path (click, marquee move,
  drag move). Enumeration happens on cache fill and FS-change
  invalidation only.
- Every repaint in these paths is masked to the diff it represents.
- Work per mouse-move during marquee or drag is proportional to
  the geometry that changed, and involves zero traps for hit-testing.
- Promise data is produced only on an accepted drop, at most once
  per flavor per item.

## 6. Acceptance battery (beyond the per-part criteria)

- SDK tests in `crates/mac-toolbox/tests` for: flavor offering and
  promise semantics (keeper not called on cancel, called once on
  accepted drop), receiver negotiation order, session ownership
  (dropping the owner cancels; using a stale session ID fails
  through the facility, typed).
- QEMU interaction tests where the harness supports them: Finder
  multi-select + drag a file between folders (move), across volumes
  (copy), to the Trash; Esc-cancel zooms back and changes nothing.
- Modifier acceptance: change Option/Command during a live drag and prove
  only the state at drop determines copy/move; test both directions, both
  modifiers held, and Trash with each modifier. A forced move from a read-only
  mount fails without a partial destination or source deletion.
- Read-only destinations reject incoming copy and move with every modifier;
  test both drag negotiation and direct Files calls, including a mount becoming
  read-only between negotiation and execution. All source/destination contents
  and metadata remain untouched on rejection.
- Transfer-window acceptance uses real managed controls/CDEFs: large and empty
  files, recursive folders and both forks; progress agrees with copied bytes,
  unrelated pixels remain unchanged, input/cursor remain responsive, and both
  Cancel and Escape stop the whole batch rather than one item.
- Escape acceptance: cancel a multi-item drag before drop and a transfer after
  partial progress; prove the entire operation is cancelled and original forks,
  metadata and identities survive, with no leftover staged files. Inject a
  rollback failure and verify it is reported rather than hidden.
- Regression: single-click selection behavior and double-click open
  are unchanged.

## 7. Out of scope / future

- Full execution of original 68k callbacks remains part of compatibility closure.
  User ruling 2026-10-21: the Drag Toolbox must retain the original API contract.
  Implement and test original selector/data marshalling against the shared store;
  do not design a native-only core or claim complete legacy execution prematurely.
- Translucent drag images; spring-loaded folders (both later
  classic features; the session model must not preclude them).
- Inter-application data drags beyond the Finder file-reference
  flavor and a plain-text flavor.
- List-view range selection (shift = contiguous range) — that's
  the list view's rule when a list view exists; icon view toggles.

## 8. DECIDEs for Chloe (defaults the agent proceeds on unless overruled)

1. **Shift-click in icon view toggles the individual item** (not a
   range) — classic Finder behavior. *Recommended: adopt.*
2. **Marquee selects items intersecting the band** (not only items
   fully contained). *Recommended: adopt (classic).* (Chloë: Yes)
3. **Get Info with a multi-item selection**: one summary window
   (Mac OS 9's "Multiple Items" behavior) rather than one window
   per item. *Recommended: adopt.* (Chloë: Yes)
4. **Native receiver notification via events** (new `Event`
   variants), not classic-style installed handler callbacks;
   callbacks are reserved for the legacy layer. *Recommended:
   adopt.* (Chloë: Yes)
5. **Drag feedback v1 is outline-only**; translucency later.
   *Recommended: adopt.* (Chloë: Yes, outline only.)

## Rust async refinement, 2026-10-21

The Rust API ergonomics spec's synchronous `take.text()` sketch is refined by
explicit user approval: promised-data takes are awaitable, with SDK event
servicing so a same-app source can produce the flavor without deadlocking.
Inline data remains immediately ready. This feeds the [base async SDK](ASYNC-SDK.md):
std-compatible event executor, runtime-independent futures, optional Tokio.
