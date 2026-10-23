# Native scrollbar CDEF1

`CDEF_SCROLL_BAR` is independently linked at `0x82200000..0x82300000` and packaged as
System `CDEF` ID1. Its source owns geometry, rendering and hit testing. The
resident Control Manager retains scalar state and a loader-validated entry;
window repaint calls the same resource with the window's origin and clip.

Clients use `system.controls().draw_scrollbar(window,bounds,value,maximum,active)`.
`draw_scrollbar_state` accepts a typed `ScrollbarState` for pressed-arrow/thumb
highlights. `scrollbar_hit` returns classic parts20/21/22/23/129. During a thumb
drag, `scrollbar_track_value` takes the unchanged mouse-down anchor and initial
value plus the latest point, returning a clamped value without an initial jump.
This relative integer mapping is our native interaction contract; its complete
behavior has not been compared with original `TrackControl`.

This first standard scrollbar supports15px and16px thickness, inferred horizontal
or vertical from the rectangle. Short bars require room for both arrow caps;
where no full thumb fits, no thumb is drawn. Maximum0 and inactive controls use
the original disabled appearance. Other thicknesses fail explicitly instead of
silently stretching the original glyphs.

The renderer generates the frame, globally phased checker track and all cap/thumb
placement. Only32 bounded square cap/thumb glyphs are extracted from an original
System7.6.1 `NewControl` proc16 probe:7,696 pixels total, with source/crop hashes
in `provenance/system7-scrollbar-assets.json`. It contains no full-window raster.
`scripts/extract_scrollbar_assets.py` reproduces the inline indexed glyphs.

`python3 scripts/check_scrollbar_pixels.py` compares every control-owned RGB
pixel for both orientations and widths at minimum/middle/maximum, inactive,
zero-range, and pressed up/down/thumb states. All32 controls/114,080 pixels match
exactly. It also compares728 original `TestControl` samples: all agree, including
arrow shadow-strip and cross-border cases. The unsampled exact midpoint split
on a thumb's outer border remains inferred; this does not claim exhaustive hit
parity at all values or dimensions. The report and difference images are under
`results/system7/scrollbar-appearance-checks.json` and `results/system7/scrollbars/`.

Host tests cover original thumb positions, hit parts, bad ABI/identity/pointers,
relative thumb tracking, safe client transport, and resource replay with a moved
origin and clip. The same no-std resource compiles and passes Clippy for RV64.
