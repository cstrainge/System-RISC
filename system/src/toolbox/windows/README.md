# Window Manager first cut

`WindowManager` owns up to 32 live windows with stable IDs, z order, activation,
outline-based drag/resize, close and zoom tracking with cancellation, local
update damage and rectangular visibility clipping. Closing or moving a front
window invalidates the exposed windows. Closed IDs are never reused during the
manager's lifetime.

The shared resident `handles::Table` supplies 16/16 generation/slot identities
and validates every lookup. Memory and windows have disjoint registered slot
ranges; a live memory handle is rejected by the Window Manager and vice versa.
Slots are reused with a new generation; stale IDs remain invalid. Window
generations stop at 255 to preserve the 24-bit native close-event payload and
disjoint desktop/offscreen target ranges. Exhausted slots are retired, never
wrapped; the current 32-slot table has a bounded lifetime allocation budget.

`new_window` uses a source-reused standard definition only in host tests.
`new_window_with_definition` instead records the loaded resource's `DefinitionEntry`
and procedure ID. The manager sends geometry, hit, initialization, disposal and
decoration calls through that entry. Procedure ID division by 16 selects the
resource; its low four bits select the variation. Custom resources never silently
fall back to the standard definition.

The `definition` module reexports shared protocol types only. Standard WDEF 0's
implementation lives entirely in `definitions/wdef/src/main.rs`, compiled as a
separate loadable resource. It reconstructs variations 0, 1, 2, 3, 4, 5, 8 and 12
from the installed System 7.6.1 reference. The manager obtains geometry and hit
parts by calling that resource. See `analysis/wdef-standard.md` and retained
`provenance/system7-wdef-reference.json`. Default title height is 19 and the grow
hit area is 14 pixels square; these choices belong to WDEF.

Native `DefinitionCall` objects use `WindowDefinitionState`, not guest 68k
WindowRecords. Messages 0–6 are draw, hit, calculate, new, dispose, grow outline
and grow icon. Hit results 0–6 map to Window Manager parts, including public zoom
parts 7 and 8. Grow outline calls require `param` to reference a live native global
`Rect` and a writable surface. State contains borrowed geometry/title data and
manager-owned zoom flags; it does not allocate a classic state handle. The definition
also supplies up to two structure rectangles: the manager preserves the actual
frame/shadow union, including its clear corner pixels, when clipping back windows.
Custom definitions supplying zero pieces retain bounding-rectangle behavior.

`draw` invokes the application painter once for each visible content fragment.
Painters must preserve the supplied clip. The System compositor can replay retained
application drawing commands into these fragments. Damage is currently represented
by bounding rectangles and broad exposure invalidation, rather than full QuickDraw
regions. The shared desktop bounds are 640×480 with the menu bar protected.

Color drawing uses the exact Chicago 12 strike and a reference-device palette
measured on the original System 7.6.1 VM. That device maps logical interpolated
RGB16 values to displayed colors; using only their high bytes would produce the
wrong appearance. The retained color-role masks reconstruct close, zoom, pressed
and grow images, with the original modal bevels, stripes and text clipping.
`DrawGrowIcon` draws its scroll separators and icon when explicitly called, including
on inactive and non-growing windows, matching the original routine.
Applications can restrict automatic grow drawing with
`Windows::set_grow_icon_clip(id, Some(local_bounds))`. Finder clips it to the
size box because its CDEF scrollbars own the adjoining borders and its information
band spans the full window width. The manager applies the clip; all drawing
still runs inside the loaded WDEF. `None` restores unrestricted drawing and an
empty rectangle suppresses it.

Run `python3 scripts/check_wdef_pixels.py` from the repository root to build the host
renderer and compare it with the controlled original-Mac captures. All **29 cases**
match exactly across **2,239,315 RGB pixels**, with zero mismatches and no masked
pixels. Coverage includes all eight public variations, active/inactive windows,
pressed close/zoom buttons, and mixed-case, long and narrow titles. The host-only
`wdef_render` example also exports individual 640×480 PPM frames for inspection.
Reference inputs and measurements live under `results/wdef-reference/`.

Remaining fidelity work includes custom window color tables, other display-device
palette profiles, alternate fonts and font-dependent title height, rounded WDEF 1,
and the 68k WindowRecord/calling-convention adapter. Pixel parity is established for
the retained reference cases and device profile. Zoom uses the native
desktop bounds as standard state and saves/restores user bounds; full classic
multi-monitor zoom policy is pending. No Apple executable bytes are embedded in
the native definition.

## Interaction acceptance

Drag and grow keep actual geometry and retained document pixels stationary while
held. The manager draws a gray XOR preview after the composed windows; a grow
preview is rendered by the loaded WDEF's `wGrow` message. Accepted release commits
one move/resize and exposure update; no-motion grow produces no resize. Releasing
a drag outside the native desktop cancels without activation. An ordinary
background drag selects its window only on successful release; Command-drag
preserves z order and activation. Close and zoom press highlighting follows
leave/reenter tracking and release outside cancels.

These contracts follow Apple's [DragWindow](https://dev.os9.ca/techpubs/mac/Toolbox/Toolbox-245.html),
[GrowWindow](https://dev.os9.ca/techpubs/mac/Toolbox/Toolbox-250.html) and
[TrackBox](https://dev.os9.ca/techpubs/mac/Toolbox/Toolbox-253.html) documentation.
The initial native desktop chooses its own bounds and grow limits; arbitrary
classic limit/slop rectangles, axis constraints, DragHook callbacks, modal policy
and multi-monitor tracking are not exposed yet. Tracking-state unit tests and
isolated real-input guest checks complement the static WDEF render corpus.
Canonical interaction acceptance now lives in
`crates/mac-toolbox/tests/window_interactions.rs`. Rust launches independent
read-only guests and asserts returned state and raw P6 framebuffers. One test
uses Finder's public SDK path; another replaces Finder only on a private clone
with a minimal application that depends only on `mac-toolbox`. There are no
resident renderer calls in the acceptance bodies or application.

The original drag and grow changed-pixel masks match exactly across **614,400
pixel positions** (762 drag pixels and 1,480 grow pixels). The minimal probe has
white content instead of the original's control samples; all mask positions are
compared, and the **2,105 identical-source pixels** additionally match the exact
original indexed XOR output colors. The initial display CLUT inverse mapping is
retained in `assets/windows/system7-indexed-xor.bin`; it was derived from the
captured 256-entry original palette and validated against original held frames.
Unknown native RGB32 colors use direct-color inversion rather than pretending
that the original indexed device defines them. Grow separators use the original
expanded-rectangle inclusive endpoints and XOR intersection cancellation.

The suite also checks held geometry/update counts, cancelled and accepted
release, leave/reentry, exact whole-frame cancellation and roundtrip restoration,
no-motion grow, Command-drag preservation and deferred background activation.
Normal zoom hit codes are `inZoomOut` (8), with `inZoomIn` (7) in standard state,
matching the original `FindWindow` trace. Original captures show pressed tracking
and the final zoom state but do not establish every intermediate frame. No
unmeasured zoom tween is substituted. Exhaustive temporal animation, arbitrary
classic bounds/callbacks, and the remaining definition families stay open gates.
