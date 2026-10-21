# Finder browser selection stall

Queued on 2026-10-20 as the next Finder performance task. Continue the current
host-directory/StuffIt driver work first; this note does not change that scope.
The authoritative implementation spec is now
[FINDER-SELECTION-DRAG.md](FINDER-SELECTION-DRAG.md), including multiple selection,
the system Drag Manager and the confirmed drop-time modifier/cancellation rules.

## Reported diagnosis

A browser click currently hit-tests by re-enumerating the folder through
per-item `Files::entry` and `text_width` traps, rebuilding the layout. It then
calls `redraw_all()`, repainting the desktop and every window, with further
enumeration and plotting. The reported cost is approximately one hundred trap
round-trips per click. Finder misses its 30-tick foreground polling window and
the System displays the watch. Record baseline traces when implementing this
fix; the diagnosis above is the user's report, not a newly measured result.

## Required change

1. Cache item information per folder: entry metadata, icon information, measured
   text dimensions and layout/hit bounds. Hit-testing and repainting use this
   cache; never remeasure text per click or per draw. Key/validate the cache using
   the File Manager's catalog revision machinery in
   `system/src/toolbox/files/scan.rs`. A filesystem change invalidates the affected
   folder's cache. Honor `ScanStale` and rebuild a consistent snapshot before
   publishing replacement cache data. Moving an item affects both folders.
2. Repaint through a mask. On a selection change, set the clip/mask to exactly
   the icon-plus-label bounds of the deselected and newly selected items. Keep
   one uniform drawing routine; vary its mask rather than duplicating a selection
   drawing path. Do not call `redraw_all()` or invalidate unrelated window or
   desktop content. Preserve normal occlusion/clipping.
3. Early-out when selection does not change. Clicking the selected item, or empty
   space when nothing is selected, performs no enumeration, text measurement or
   repaint. Selection handling itself must not rebuild or validate the whole
   directory on each click; cache invalidation follows actual catalog changes.

## Acceptance

Changing browser selection costs no folder enumeration, repaints only the masked
items and never displays the watch. Repeated no-change clicks do no drawing work.

Add Rust-owned acceptance assertions in `crates/mac-toolbox/tests`, with private
VM capture scripts transporting evidence. Trace enumeration/text-measurement and
paint work before/after selection; verify unchanged pixels outside the selected
item bounds and responsiveness with a populated folder and other windows open.
Also verify that a real folder mutation invalidates its cache and that stale scan
results cannot replace a current snapshot. Use Cursor Test responsiveness and
`present()` timing traces as the performance checks.
