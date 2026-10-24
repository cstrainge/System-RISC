# Cursor services

`system.quickdraw().cursors()` is a thin public SDK over resident, per-application
cursor state. `set_standard(StandardCursor::Arrow)` and `init()` enable automatic
editor hover; `IBeam`, `Crosshair`, `Plus`, `Watch`, custom images and explicit
resources keep their selected shape until the application restores the arrow.
`init()` also resets visibility nesting and obscuring, as original InitCursor does.

The System contains every original CURS resource from the pinned System fork,
and Finder retains its original CURS resources. Both original `acur` sequences
are packaged. The arrow is the exact QuickDraw global record from the ROM,
including its hotspot; it is not invented as CURS0. Source identities, byte
offsets and hashes are in [cursor provenance](../system/assets/cursors/provenance.json).
Run `python3 scripts/extract_system7_cursors.py --check` to verify the imports.

The public API supports scoped `.system()`, `.application()` and `.search_chain()`
lookup, `set_resource` for CURS, `set_color_resource` for indexed crsr, and
`animate_resource` for acur. Animation frame dependencies resolve in the chosen
scope with a System fallback, preferring color frames. A zero interval uses
`advance_animation`; other intervals count 60 Hz uptime ticks. Timed animation
wakes an idle event consumer only for a visible frame change and posts no events.
Hidden animation state continues to advance. Indexed 1/2/4/8-bit color cursors,
mask compositing, source XOR, clipping, hotspots, malformed resources and atomic
failure preservation are handled by the resident service. The pinned original
forks contain no crsr; color decoding tests use explicitly synthetic records.

Hide/Show nest with a maximum visible level of zero. Obscure does not alter that
level and Show does not cancel it. Actual pointer motion clears obscuring across
application contexts, even when queued movements return to the initial position.
Shield always decrements the level, subtracts the supplied port offset from its
rectangle, and defers hiding a nonintersecting visible cursor until motion.

TextEdit automatically binds a successful draw's editor, window, view, origin
and exact clip region. The foreground compositor selects the original I-beam
only over exposed editable content. Keyboard focus is independent of hover;
`set_cursor_enabled(editor, false)` disables it for read-only fields. Window
chrome, occlusion, menus and WDEF tracking exclude automatic editor selection.
Successful user editing/navigation obscures the cursor; programmatic text and
layout changes do not. Hover regions are committed with the retained scene at
EndUpdate; a replacement scene also removes any editors it omits. Disposing an
editor or its window expires the binding.

Compatibility follows [Inside Macintosh, Cursor Utilities](https://developer.apple.com/library/archive/documentation/mac/pdf/Imaging_With_QuickDraw/Cursor_Utilities.pdf)
and [Apple TB03](https://developer.apple.com/library/archive/qa/tb/tb03.html).
The existing classic Set/Init/Hide/Show/Obscure/Shield argument adapters share
these services. Original resource-handle GetCursor/GetCCursor lifetimes and the
complete classic Color QuickDraw trap adapter family remain part of legacy ABI
work; the native resource API does not claim those adapters are implemented.

Canonical acceptance lives in `crates/mac-toolbox/tests/cursors.rs`,
`textedit_cursors.rs`, and `cursor_graphical.rs`. The graphical test launches
the installed Cursor Test via Finder in a private guest and compares exact
original sprite pixels, timed watch frames, automatic text hover, read-only
fields, obscuring, out-and-back movement, title-bar and menu behavior. The
transport preserves base disk hashes and never sends input to the visible VM.
`retained_update.rs` additionally checks hover-region publication while two
actual application threads are running.

The cursor is composited into the frontbuffer during damaged-rectangle
presentation. The scene is borrowed read-only; cursor pixels never enter the
backbuffer. Pointer-only presentation copies the old cursor bounds from the
scene and publishes the new bounds with the current sprite, without replaying
window content or sweeping scene visibility. Scene damage underneath a stationary
cursor uses the same masked blit. Monochrome inversion, color masks, clipping,
hotspots, visibility and animation retain their original semantics.

Drag outlines and highlights still temporarily paint and restore the canvas;
they have not yet migrated to present-time compositing. Presentation remains
serialized with the compositor; this does not establish independent pointer-path
scheduling or a strict input-latency guarantee. A failed scene replay suppresses
cursor-only copies until a complete scene is available again.

The graphical cursor test snapshots all 1,228,800 backbuffer bytes around an
actual pointer move, asserts byte identity and unchanged scene/update/menu
counters, and checks exact sprite pixels and unrelated screen bounds. SDK tests
also cover scene damage, overlapping rectangles, all mono/color mask operations,
surface refusal before any write, hide/show, shape changes and clipped edges.
