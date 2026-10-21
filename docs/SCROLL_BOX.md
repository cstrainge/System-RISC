# Toolbox scroll boxes

`System::controls().create_scroll_box(window, spec)` creates an owned virtual row
viewport with a CDEF1 scrollbar. The safe `no_std` SDK lives in
`toolbox/controls/src/scrollbox.rs`; the resident model is
`system/src/toolbox/controls/scrollbox.rs` and forbids unsafe code. CDEF1 retains
ownership of scrollbar geometry, drawing, hit parts and thumb mapping.

The parent window and caller own the control. Its ID uses the shared handle
facility's disjoint ScrollBoxes namespace. Drop disposes it; closing the parent
or exiting the app removes it. Stale, foreign-service and cross-owner IDs are
validated for both native and Pascal-string trap callers.

```rust,ignore
let view = system.controls().create_scroll_box(window, spec)?;
let control = view.control();
for row in control.info()?.missing {
    control.set_row(row, &format_row(row), false)?;
}
control.draw()?;

let update = control.scroll_to(1)?;
for row in update.missing {
    control.set_row(row, &format_row(row), false)?;
}
control.draw()?;
```

`View::missing` is an absolute row range. One-line scrolling shifts retained
pixels and normally requests only one row. Page/jump scrolling requests the
exposed range, bounded by the viewport. A partial trailing row remains blank;
`View::rows()` counts complete rows. Configuration changes invalidate the
viewport and clamp its offset. `invalidate` requests a fresh visible range.
Rows carry UTF8 text and optional selected colors; text is clipped to its row.
This initial control is a vertical text-row viewport, not an arbitrary child
widget container or the classic List Manager.

`track(anchor, callback)` pumps physical input and asks the application to fill
missing rows. Resident tracking owns arrow/page actions, initial delay, repeat
timing and live thumb movement. Dragging maps the classic CDEF's 16-bit value
onto the complete 32-bit row range, preserving both endpoints. The callback
does not decide geometry or draw the scrollbar. Offscreen controls use
`WindowId(0)` and `draw_pixels`; tracking requires a real parent window.

DRAW publishes an immutable raster snapshot only after every required row is
valid. Incomplete publication returns `SERVICE_BUSY`; callback/allocation errors
leave the previous committed image available. The raster is an immutable table
of independently owned row tiles. Updating a shared snapshot copies only the
small table; supplying text allocates/reuses only that row. Scrolling rotates
row references and invalidates the exposed range, without copying viewport
pixels or rerasterizing unchanged text. Configuration and invalidation allocate
no full-viewport pixel buffer; blank and partial trailing rows use the retained
background color. The
compositor replays viewport blits directly instead of keeping a duplicate
whole-window cache. Viewport double-buffer reserves and whole-window caches
share a 2 MiB budget; allocation failures return `NO_MEMORY`. Shared row/table
ownership uses `Arc::try_new`, including the small reference-count allocation.
Fault-injected public calls verify both allocation stages return an error and
leave the control usable; they cannot abort the kernel on shared-owner OOM.

A stable DRAW damages only the viewport and scrollbar, never the parent window
or neighboring controls. Moving/reconfiguring or disposing a published control
also damages its previous bounds. The compositor clips replay to visible damage;
fixed headers remain outside it, and bitmap text outside the damaged vertical
span is rejected before glyph rendering. Cursor save-under is independent of
the scroll-box raster.

Current limits are 96 controls, dimensions up to 2048 pixels subject to that
shared budget, a 16-pixel scrollbar, positive fixed row height, and 768 UTF8 bytes
per supplied row. Apps may choose fonts, padding and normal/selected colors.
The resident interface is native trap `SCROLL_BOX` (`0x10021`), with a companion
`PSTRING_SCROLL_BOX` trap for MacRoman Pascal row text. Public Rust APIs expose
only UTF8. This adds no classic 68k adapter or claim of complete TrackControl
compatibility.

Canonical public-SDK tests are `crates/mac-toolbox/tests/scrollbox.rs` and the
disk-booted `tests/conformance/ownership.rs` suite. They cover exact pixel
equivalence after scroll/resize/jumps, exposed-row ranges, large endpoints,
UTF8/Pascal parity, incomplete draws, stale IDs after forced slot reuse, owner
checks, parent cleanup and Drop. An allocation-instrumented public test creates,
reconfigures, fills and scrolls a 300-by-300 viewport while refusing allocations
above 64 KiB, reproducing the contiguous-allocation pressure that previously
failed ResEdit despite free heap space. The native retained-update test proves
that changing one shared row stays invisible until EndUpdate and leaves other
rows intact. ResEdit's real-app test checks independent
navigation panes, Monaco pixels, radio CDEF animation, native JSON saving and
one-row rendering timings. Python supplies input/captures; Rust owns acceptance.
