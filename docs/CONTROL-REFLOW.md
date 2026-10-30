# Native control reflow

Reflow is an immutable creation choice (`Reflow::Yes` or `Reflow::No`).
Controls retain creation size, calculated size requirements, explicit overrides
and assigned geometry separately. `ask_size` measures; `assign_rect` prepares and
publishes geometry. Setters mark dependent work pending; they do not resize a
control immediately. Window-root scheduling and scrolling containers are not
provided by these APIs yet.

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
transaction before retrying. Splitter controls and interactive divider resizing
are separate, deferred facilities.

## Tracks and anchors

Rows and columns default to `TrackSize::Fit`. `set_column` and `set_row` select
`Fit`, `Fill`, or `Percent(hundredths)`; 10,000 means 100%. Percentages on an axis
must total at most 100%. Selecting Fit restores the implicit default. A
configured nondefault track participates even if it currently has no child.

Table's CDEF calculates each axis as follows:

1. Aggregate single-track minima and preferred sizes, then the requirements of
   spanning children in stable cell order. Split a span's unmet requirement
   evenly across its tracks, giving remainder pixels to earlier tracks.
2. Reserve minima. Grow percentage tracks toward their fraction of the offered
   budget; minima take precedence and competing deficits share available space.
3. Grow Fit tracks toward preferred. Give remaining space equally to Fill tracks.
   Without Fill, unused space remains after the final track.
4. When the assigned budget is below the combined minima, shrink proportionally
   to those minima. Cumulative integer rounding spends the entire available
   budget deterministically. Zero is a valid assigned extent. The measurement
   reply still reports the true minima; shrinking does not rewrite them.

`ControlRef::set_anchors(Anchors { left, right, top, bottom })` is shared by all
reflow controls. Anchors apply independently on each axis within the parent's
allocated cell or merged region:

- Neither edge: center at preferred size, limited to the available space.
- One edge: retain that size and pin to the selected edge.
- Both edges: stretch across the allocated space.

`Anchors::NONE` is the default; `Anchors::ALL` stretches in both directions.
Changing anchors preserves current geometry until the parent's next assignment.
A nested Table is anchored like any other child and arranges its own children
within the rectangle it receives. Drawing and hit testing use the resulting
resident tree geometry and ancestor clipping. Dotted outlines show occupied cell
regions, including merged regions without internal dividers.

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
invalidation and manual child assignment mark ancestors pending. Current fixed-content leaf families do not implement wrapped text;
the constrained protocol does not itself establish wrapping support.

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
an overflow into apparent success. Wider logical scrolling extents, scrollbar
coupling and window-root scheduling remain separate work.

## Evidence boundary

Native scenarios live in the existing disk-loaded Toolbox fixture. They exercise
real target calls and loaded CDEFs; no host substitute qualifies these paths.
Recorded artifact, configuration, runtime observations and remaining gaps belong
in `status.txt`. A compile or a successful bounded run is not whole-OS, race,
physical-hardware or Windows qualification. The repository remains unvalidated
outside its specifically recorded observations.
