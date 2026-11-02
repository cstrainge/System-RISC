# Native control reflow

Reflow is an immutable creation choice (`Reflow::Yes` or `Reflow::No`).
Controls retain creation size, calculated size requirements, explicit overrides
and assigned geometry separately. `ask_size` measures; `assign_rect` prepares and
publishes geometry. Setters mark dependent work pending; they do not resize a
control immediately within a Table. The next application event boundary consumes
pending root work. Explicit window anchors additionally arrange top-level controls
immediately and on accepted resize/zoom. Scrolling remains separate work.

## Table cells and spans

`Table` owns its hosted controls. `TableRef::set_child(Cell { row, column }, child)`
adopts a single cell. `set_child_spanning(cell, Span::new(rows, columns)?, child)`
adopts a rectangular region. Spans are nonzero, their exclusive endpoints must
fit `u32`, and every covered cell must be free. A span is attached to one real
control; there are no placeholder controls or second ownership registry.

Adoption failure returns `AdoptionError { cause, child }`, retaining the original
owner. `remove_child` returns ownership. `reparent_spanning` changes parent,
position and span in one transaction, including changing a span within the same
Table. `reparent` places the child in a single cell; use the spanning operation
to retain or change a larger region. Overlap returns `AlreadyExists` without
moving either child. Same placement and span is a no-op. Child enumeration is
row-major and includes spans; relationship changes invalidate existing cursors.

Children must opt into reflow and share the parent's window. Cycles, stale or
foreign identities, unfinished paint transactions and captured subtrees are
refused before relationship changes. Cancel capture or finish the paint
transaction before retrying. Interactive dividers use the loaded Splitter CDEF
and the same managed capture/publication boundaries.

## Tracks and anchors

Rows and columns default to `SizeBounds::CONTENT`: no explicit bounds and
`Fill::None`. `set_column` and `set_row` accept `SizeBounds`; `column` and `row`
return the accepted settings. `SizeBounds::FILL` is the unconstrained fill choice.
`SizeBounds::new(min, max, fill)` validates the typed settings. Its fields are
private; `minimum()`, `maximum()` and `fill()` return copied values.

```rust
let column = SizeBounds::new(
    Some(SizeUnit::Pixels(80)),
    Some(SizeUnit::Percentage(60.0)),
    Fill::Fill,
)?;
table.set_column(0, column)?;
```

Pixel values must be nonnegative. Percentages are finite, nonnegative `f32`
values, with `100.0` meaning the complete offered Table axis after splitter
gutters; fractional pixels round down. Percentages are independent bounds,
not shares whose sum must be at most 100%. The offered axis is the Table's
assigned rectangle, before optional scrollbar chrome. Intrinsic measurement
has no finite percentage basis, so relative bounds defer to finite assignment;
pixel bounds still apply. Contradictory same-unit bounds refuse immediately;
a mixed-unit contradiction or unrepresentable result refuses when resolved.

Table's CDEF calculates each axis as follows:

1. Aggregate single-track content minima and ideals, then spanning requirements
   in stable cell order. Internal splitter gutters count toward a span's space.
2. Apply each track's bounds. An explicit minimum replaces its content minimum;
   an absent minimum uses the content minimum capped by an explicit maximum.
   Clamp the content ideal (or retained manual split size) to those bounds.
3. Reserve minima. Grow `Fill::None` tracks toward their clamped ideals, then
   share remaining space equally among `Fill::Fill` tracks without exceeding
   maxima. Capped tracks leave the remaining distribution to eligible tracks.
   If all tracks reach their maxima, unused space remains after the last track.
4. When the budget cannot satisfy all minima, shrink them proportionally, as in
   the existing forced-size policy. Integer rounding is deterministic and zero
   is valid. The measurement reply still reports the true minima. Maxima remain
   enforced. Under extreme shortage, the splitter gutters also shrink safely.

Changing a row or column's bounds clears manual split sizes on that axis and
schedules ordinary reflow. An unchanged request does no work unless it clears
such manual sizing. A captured Table refuses a bounds change with `ServiceBusy`.
Previously accepted geometry remains usable while new settings await layout.

`ControlRef::set_anchors(Anchors { left, right, top, bottom })` is shared by all
reflow controls. Anchors apply independently on each axis within the parent's
allocated cell or merged region:

- Neither edge: center at preferred size, limited to the available space.
- One edge: retain that size and pin to the selected edge.
- Both edges: stretch across the allocated space.

`Anchors::NONE` is the default; `Anchors::ALL` stretches in both directions.
Changing anchors preserves current geometry until the parent's next assignment,
including the automatic assignment at the next application event boundary.
A nested Table is anchored like any other child and arranges its own children
within the rectangle it receives. Drawing and hit testing use the resulting
resident tree geometry and ancestor clipping. Dotted outlines show occupied cell
regions, including merged regions without internal dividers.

`table.table().set_grid_lines(GridLines { rows, columns, border })` independently
controls horizontal row separators, vertical column separators and the outer
Table border. `GridLines::ALL` is the default; `GridLines::NONE` hides all three.
`grid_lines()` reads the current setting. The background and hosted controls
remain visible. This property also works on non-reflow Tables (which have only
an outer border). Merged cells never gain lines through their interior, and cell
edges along the Table perimeter do not recreate a disabled outer border.

Changing these switches atomically redraws the Table using its existing cell
geometry. It does not measure or arrange children, dirty layout caches, change
hit testing or cancel a child's capture. Repeating the current setting does no
drawing work. An admission failure retains the prior settings and snapshot;
the caller can release pressure and retry. During an open window update, the
new snapshot participates in that existing paint transaction.

## Owning-window borders

On a top-level `Reflow::Yes` control, `set_anchors` uses its owning window's
**content** borders (local `(0, 0, width, height)`), excluding the title bar and
frame. Nonempty anchors opt the root into immediate placement and subsequent
window resize/zoom. Opposing edges stretch; a single edge pins the preferred
size; an axis with neither edge centers. Available space can shrink the preferred
size below its minimum. `Anchors::NONE` disables automatic window placement and
preserves the current rectangle. Unconfigured roots keep their existing geometry.

`set_window_border(BorderOffsets::new(left, right, top, bottom)?)` supplies
nonnegative per-side offsets. All offsets default to zero, so anchors align
directly to the content borders. Offsets reduce the available rectangle before
pinning, centering or stretching. If they consume an entire axis, its extent is
zero; the placement stays within the window. `BorderOffsets::ZERO` clears them.
`ControlInfo::window_border` reports the property. Setting the property on a
Table child or a control without reflow is refused. This is placement spacing,
not a request to draw a border.

For example, set a root Table's border offsets to twelve pixels on every side,
then call `table.control().set_anchors(Anchors::ALL)`. System arranges the Table
inside that inset rectangle and the Table arranges its children normally.
Table children continue to use cell/span anchors. Adoption suspends window-root
placement; a removed child becomes eligible for it again at the next event
boundary. Reapply its anchors to arrange immediately. Window movement leaves all control-local rectangles intact.

Grow and zoom prepare the proposed window geometry and every active root subtree
before publishing. An unfinished paint transaction, active affected control
capture, unrepresentable geometry or resource refusal preserves the old window,
zoom state, control settings and all control rectangles. The gesture is released
and may be retried; the diagnostic retains the status and a contextual cause.
Explicit anchor/border setters return the typed error to their caller. Cancel
capture or finish painting before retrying. A repeated clean setting at the same
window size does no layout allocation and preserves capture.

Size/content setters mark work pending for the next event boundary. Reapplying
root anchors or an accepted size change can also consume that work immediately.
Content-derived window minima and scrolling policy are not supplied here.

## Automatic content reflow

Changes to content, sizing preferences, anchors, tracks or tree membership
coalesce until the owning application next retrieves or peeks at events. A
positioned root Table arranges within its existing rectangle. A root with active
window anchors recomputes its placement against the current window content area.
Unanchored standalone leaves retain manual placement. Root Table/leaf geometry
never changes the window's size. No application callback or layout worker runs.

The Control Manager keeps inline work state in its existing registered per-owner
storage. A clean event poll does not scan the tree, allocate scratch or invoke
definitions. A pending pass snapshots only eligible dirty root IDs, then stages
each root under the normal desktop guard and the polling application's actual
allocation authority. Roots publish independently: a blocked root does not
prevent an unrelated root from progressing. Cached measurements remain reusable;
unchanged geometry produces no retained drawing replacement. Protected recovery
services do not execute ordinary application layout work.

An open paint transaction or captured subtree defers its root before plan
allocation. Paint completion or capture release wakes it for the next event
boundary. Allocation refusal retains previous geometry and pending work; retry
is limited to once per 60 uptime ticks unless a relevant setting changes or the
window finishes a paint transaction. Native waits are shortened to the pending
retry deadline so a long event timeout does not strand work. Other preparation
errors wait for a layout edit or explicit invalidation. Applications still need
to poll events; no layout runs on behalf of an application that stops polling.

`ControlInfo::layout_error` reports the last automatic refusal of the control's
current root, including `ServiceBusy`, `NoMemory` or unrepresentable dimensions.
Successful layout clears it. `arrangement_pending` records pending geometry;
previously accepted content/settings remain committed even if their subsequent
layout is refused. Explicit assignment continues to return typed failures
synchronously. Root errors emit bounded transition diagnostics, and deletion or
reparenting uses the current authoritative tree rather than a retained work
owner or stale queue identity.

## Measurement and publication

The shared `sr-control-reflow-handling` crate defines validated sizing and
placement types and the borrowed calculation contract. The Table CDEF owns grid
allocation policy. System owns objects, configuration, measurements, scratch
admission, retained output and publication. Application wrappers do not calculate
positions or keep a parallel child tree.

An operation obtains intrinsic requirements bottom-up, resolves widths top-down,
measures children at those widths, then resolves row requirements bottom-up.
Assignment subsequently resolves heights top-down. These are bounded stages,
not a negotiation loop. Cached leaf measurements and intrinsic container answers
are reused. Clean nested grids also reuse assigned axes when their size is
unchanged; moving a grid translates those results. Explicit child arrangement
invalidation and manual child assignment mark ancestors pending. Static text derives natural requirements from the selected resident font.
`StaticTextSpec` selects `TextLayout::Wrap` or the default `Ellipsis`, independently
of the immutable `Reflow` choice. Wrap preserves CR, LF and CRLF hard breaks and
blank lines, prefers whitespace breaks, and splits oversized tokens only at
extended grapheme boundaries. NBSP and narrow NBSP do not form soft breaks.
Nonempty static text reports an ellipsis-width, one-line minimum. Native
state validation refuses font widths beyond the local coordinate range; classic font
queries retain their saturating result. The constrained answer reports height at the resolved width; zero width still
makes bounded progress. A height shortage clips output rather than changing
requirements. Ellipsis preserves hard lines and fits a grapheme-safe prefix plus
an ellipsis; if even the ellipsis cannot fit, that line paints no ink.

`StaticText::into_control` transfers ownership for Table adoption. Borrowed
`ControlRef::static_text()` setters update text, font, alignment, layout mode or
bounds atomically in resident storage. Unselected properties retain their latest
values; a text update never restores the creation rectangle. Unchanged updates
avoid drawing publication. Invalid properties and retained admission failures
preserve the prior complete state. Successful content/style changes invalidate
measurement and schedule the existing event-boundary reflow. Other current leaf
families retain their documented creation-size fallback.

System stages the affected subtree while holding the existing desktop service
guard. No object-store mutable borrow crosses a definition call. Definitions
receive safe borrowed scratch views; their narrow native boundary and admitted
services cannot recursively mutate controls. The loader's cache owns the invoked
images for the duration. Plans do not escape this synchronous guard.

Geometry, capture state, clips, immutable cell outlines and retained snapshots
are admitted before publication. Resource failure releases staging and preserves
the previous usable subtree. Previously committed settings remain pending for
retry. Publication does not perform fallible allocation or CDEF calls. Painting
uses the published snapshots afterward. An unchanged assignment of a clean
Table returns before allocating a plan, loading definitions or cancelling capture.
A pending paint transaction refuses subtree assignment until EndUpdate.

Calculations use checked wide arithmetic, then validate conversion into local
`Dimensions`/`Rect`. This API currently refuses aggregate requirements or local
geometry outside their representable range; it does not truncate, wrap, or turn
an overflow into apparent success. Scrolling retains wide logical extents while visible CDEF rectangles remain finite.

## Evidence boundary

Native scenarios live in the existing disk-loaded Toolbox fixture. They exercise
real target calls and loaded CDEFs; no host substitute qualifies these paths.
Recorded artifact, configuration, runtime observations and remaining gaps belong
in `status.txt`. A compile or a successful bounded run is not whole-OS, race,
physical-hardware or Windows qualification. The repository remains unvalidated
outside its specifically recorded observations.

### Fixed-content Table scrolling

`TableRef::set_scrolling(Scrolling::Fixed { horizontal, vertical })` opts into a
viewport whose content uses finite preferred track sizes, clamped by track
bounds. Fill distribution does not expand this fixed content. Percentage bounds
follow the offered Table dimension and therefore require recalculation when that
dimension changes. At least one axis must be enabled;
`Scrolling::Disabled` restores the fitting policy. A disabled axis clips with
position zero. The scrolling viewport reports its own minimum and creation-size
preference instead of the document's total dimensions. Explicit size overrides
continue to apply.

Bars appear only on overflow. System evaluates the four possible bar sets
against the reduced viewport, selecting the least consistent set in stable
horizontal-before-vertical order. A vertical bar can induce a horizontal bar.
This calculation does not remeasure fixed content. Tiny viewports clip safely;
scrollbars too small for the loaded CDEF's geometry are noninteractive.

`scroll_info()` reports the accepted viewport, wide content extent, bar visibility
and position. `scroll_to(ScrollPosition { x, y })` rejects negative positions and
clamps positions beyond the content. `reveal(child)` minimally exposes a direct
child, aligning its leading edge if the child is larger than the viewport.
`content_rect(child)` returns the last accepted rectangle in parent-content
coordinates; pending content changes have not yet changed that rectangle.

A clean scroll only translates and clips cached child geometry. It does not ask
children to measure again or change their logical arrangement. Content edits
still use ordinary automatic reflow; scrolling a dirty Table first prepares its
layout. This fixed-content policy does not automatically wrap content to the
viewport width. A width-following scrolling policy is not implemented.

Content positions and summed extents use signed 64-bit coordinates. Individual
CDEF sizes and partially visible drawing rectangles must still fit the native
QuickDraw coordinate contract. System refuses an unrepresentable visible
rectangle with `BadArgument` before publication. Fully offscreen controls retain
finite local geometry and an empty clip; their logical rectangle remains
available through `content_rect`, while ordinary `ControlInfo` bounds describe
the current drawing representation. Assign a rectangle to a removed offscreen
child before using it independently. Reparenting preserves accepted screen
placement until the destination's pending layout commits, including a nested
Table scrolled before that event boundary.

Mode changes, scrolling and capture transitions stage state and retained output
before publishing. Refusal preserves accepted geometry, position and capture.
Pending window painting or another captured child returns `ServiceBusy`; retained
or scratch allocation refusal returns `NoMemory`. Complete painting or cancel
capture before retrying. Capture and highlight cleanup allocate no memory; existing damage collection
can attempt allocation and falls back on refusal. Bars use the
standard loaded scrollbar CDEF and resident tracking state machine. Hit testing
may return the Table itself for a bar, and its ordinary managed `track` method
performs the scroll. Arrow steps are 16 pixels, pages overlap by 16 pixels, and
thumb positions map the CDEF's bounded range onto the wide content range.

### Splitters

`Controls::create_splitter(window, SplitterSpec { .. })` creates an independent
managed divider using `CDEF_SPLITTER`. A vertical divider moves horizontally;
a horizontal divider moves vertically. The optional inclusive `range` limits
its reported coordinate. `parent_bounds` optionally supplies the parent's
window-local rectangle: a vertical drag outline spans that height, while a
horizontal outline spans that width. This does not itself limit movement.
Without parent bounds, the outline uses the divider's own length.

During capture only the outline changes; the divider and panes retain accepted
geometry. Release reports the final coordinate through the normal control action
callback (`ControlInfo.standard.value`, or `SplitterRef::track`). The parent
applies its own resize policy. Cancellation removes the outline without a CDEF
call or drawing allocation. Accepted changes to bounds, orientation, parent
bounds, range or enabled state cancel the old capture; refused changes preserve
it. Automatic cursor selection uses the published divider geometry and normal
window/control clipping. Explicit application cursor policy still takes priority.

A Table independently enables `set_column_splitter(after, enabled)` and
`set_row_splitter(after, enabled)` at zero-based boundaries; all default off.
These are instances of the same loaded Splitter definition and resident capture
mechanism, owned by the Table. They do not introduce another object-ID registry
or application-side drag loop. Enabled interior boundaries reserve a four-pixel
gutter. Spanned cells omit the divider segments crossing their merged region.
The Table supplies its viewport as parent outline bounds and derives the drag
range from both adjacent tracks' resolved minima and maxima.

On release, the Table stages new track sizes, child geometry, clips and retained
output together. Only accepted publication changes panes. `NoMemory` or other
refusal preserves the old panes and capture for retry or cancellation. A drag
retains explicit requested sizes for that axis; later changes clamp them to
current bounds. `reset_column_split_sizes()` and `reset_row_split_sizes()` restore
content/fill distribution. Editing track bounds also resets that axis. Clean
scrolling translates the accepted splitters along with content without measuring
again. Bounds changes, content changes and viewport-relative constraints use
ordinary staged reflow.

Control Suite uses two rows and two columns, with their middle boundaries enabled.
Columns have an 80-pixel minimum and rows a 60-pixel minimum, with fill enabled.
The root follows the window with a 12-pixel inset, and managed tracking applies
both dividers without an application hit/press/release loop.

The resize cursor resources are native monochrome artwork with editable provenance
under `system/assets/cursors/resize-provenance.json`. Horizontal identity 17 is
recorded in Apple's [Appearance header](https://raw.githubusercontent.com/phracker/MacOSX-SDKs/master/MacOSX10.1.5.sdk/System/Library/Frameworks/Carbon.framework/Versions/A/Frameworks/HIToolbox.framework/Versions/A/Headers/Appearance.h);
vertical identity 21 follows the later Carbon cursor. These are native resource
choices, not claims that the original System CURS resource numbers matched those
ThemeCursor identifiers. They remain replaceable assets.
