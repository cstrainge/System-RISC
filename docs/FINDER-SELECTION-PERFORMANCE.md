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
   Use resident [file notifications](FILE-NOTIFICATIONS.md) for open-folder
   invalidation, including changes made by other apps or outside the guest.
   Read-only mounts accept subscriptions even when their watches remain quiet.
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

## Browser milestone verified on 2026-10-21

Folder windows now use bounded catalog snapshots, cached icon descriptors and
wrapped Geneva captions. Hit tests and selection repaints perform no Files or
font-width traps. A variable-height interval index includes every caption line;
selection changes publish rectangle-mask patches through the public SDK. Shift
toggle, marquee union/shrink/cancel, Select All and Multiple Items Info share
the set model. In-flight scan invalidation preserves selected identities, and
physical press/release pairs received during a refresh survive publication.

The canonical `finder_selection` guest test checks 16 captured stages, exact
pixels outside selection bounds, wrapped-label hit testing, no-change early
outs, scan races and 2,000 outline moves. In the verified private run, those
moves added 2,001 presentations with **zero scene compositions or menu paints**.
Feedback uses a temporary resident save-under overlay; it does not append
retained drawing history or invalidate the window raster. Reclaimable pixel
caches preserve heap headroom for other application resources and thread stacks.

Supplementary `finder_layout` tests cover UTF-8/grapheme wrapping, the 255-byte
name limit, variable row height and tall-label interval queries. Private native
application launching/switching, Get Info persistence across cold boot and the
aggregate SDK conformance gate also passed. This is not a claim of complete
Finder gesture coverage or a fresh original-system multiline visual comparison.
The desktop now also caches mounted-volume names, wrapped captions and geometry,
and shares the interval index and selection set. Modifier clicks, marquee
cancellation, Select All and multi-selection press/release semantics use masked
desktop transactions. Selection activation repaints only selected items; the
asynchronous deactivate edge from a previous folder does not erase a desktop
double-click. Explicit mount discovery refreshes reconcile selected volume IDs.
Desktop error subjects come from the same cache, including wrapped captions.

The expanded guest gate checks 28 stages and 4,000 outline moves across windows
and desktop, with zero enumeration/measurement calls on those pointer paths.
It also verifies volume double-clicks across the asynchronous deactivate edge.
Native mounted-provider browsing/New Folder and rapid close regression checks,
application launching/switching, and cold-boot Get Info persistence passed.
The File Manager now persists icon positions in each object's Finder metadata,
preserving dates, forks and unrelated bytes. Same-volume moves retain the CNID,
complete Finder metadata and comments; the native guest gate closes/reopens and
renames a placed file, moves it into another folder, and checks that Finder drops
the source icon and renders the destination at its saved position. Shared SDK
acceptance on HFS and HFS-B also covers folder/child identities, remounts,
collisions, cycles, no-op scans/watches, open-file protection and rollback after
partial write/flush faults. Legacy replies share the same services and only
project their returned names to Str31. Read-only providers reject mutations.

Writable provider metadata/moves, resident Drag Manager, drop coordinate/group
placement and batch copy/move/progress remain parts of the authoritative spec.
The storage API is not a claim that the complete drag UI is implemented.

## Catalog changes and saved placement, 2026-10-21

An invalidated folder keeps its last rendered pixels while a new bounded scan
runs. Finder retains one shared baseline for the active scan, not a second
catalog per open window. Unchanged names reuse their measured captions. At
publication, identity comparisons determine the old/new item masks; count and
scroll-range changes update only the information band and managed scrollbars.
There is no blank intermediate frame or full-window update for a catalog change.

Before placing an unpositioned file, reserve every existing icon and its complete
wrapped label. Assign a free automatic cell, then persist the new positions as
one native catalog batch. Initial views and files created by another application
use this same path. Existing positions remain unchanged when an item departs.
Object dates, forks and unrelated Finder metadata are preserved. Immutable
volumes retain stable placements in the current view without attempting writes.

The native Finder selection fixture now checks 30 graphical stages. Its final
cases create unpositioned files, verify saved positions and nonoverlap, move an
item out, require zero full-window updates and no remeasurement of unchanged
names, and compare every pixel outside the departed item/header/control damage.
The physical drag gate also verifies persisted initial application placements.

The external provider still lacks the complete metadata/move contract. Its
current lack of position persistence is explicit unfinished driver work, not an
acceptable final writable-filesystem contract. Folder-window bounds must also be
stored on the directory and restored on reopen across every writable backend.
