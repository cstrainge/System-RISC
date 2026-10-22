# Drag Manager implementation and compatibility

System owns one generation-checked drag manager for native and original trap
callers. The SDK exposes a sole RAII source owner, borrowed tracking metadata,
drop-only take-once data and typed terminal outcomes. Original counted-byte data
reads remain repeatable; they are not native takes. Source/receiver thread and
owner checks are separate from handle generation checks. Definitions and feedback
never execute application callbacks in another application's context.

## Current verified slice

`mac-toolbox/tests/drag.rs` exercises real resident services through the SDK and
the original Pascal A-line gate: lazy same-app producers, take-once, duplicate
pending takes, cancellation wakes, source ownership, WrongThread, stale-reference
slot reuse, one-based original enumeration and partial repeatable flavor reads.
Native `TEXT` is UTF-8. Original `TEXT` sizes, reads and set offsets use MacRoman;
conversion is counted bytes, without a Pascal-string length limit. Unrepresentable
native text fails classic access without returning UTF-8 as MacRoman. A rejected
producer payload completes the promise with an error instead of stranding a take.

`mac-toolbox/tests/finder_drag.rs` drives real QEMU pointer input against Finder.
It covers single/group same-folder repositioning, relative offsets, Escape,
cold boot and persistent catalog placement. Rust reads the resulting disk through
the public Files SDK. During the tracked pointer-motion interval it requires zero
file/font calls and unchanged scene composition, menu painting and update counts.
Feedback uses resident outline/highlight save-under overlays beneath the cursor.

The broader native regression run currently stops in ResEdit: repeated resource
viewer changes exhaust available memory while configuring/drawing a scroll box
(`NO_MEMORY`). The drag acceptance tests pass; the full suite is not green.
Investigate retained viewer/raster lifetimes separately rather than increasing
the heap budget to conceal the failure.

Native offers carry their original parent identity. A group move validates all
expected parents and publishes placement/catalog changes in one transaction.
Repositioning does not re-query each source file at drop. Closed-folder/volume
drops preserve relative layout in destination coordinates. There is no app-local
pointer tracking or direct source deletion on a successful receiver outcome.

Finder cross-volume copying, Option-forced copies, transfer windows/rollback,
desktop file placement, volume-icon repositioning and text-field clients remain
unfinished. The resident policy already samples modifiers at release: Command
forces move, Option copy, Command wins, Trash always move, read-only sources are
copy-only and read-only destinations reject. A policy test is not an implemented
filesystem transfer. The general std-compatible executor and guest std/Tokio
support remain the separate gates in [ASYNC-SDK.md](ASYNC-SDK.md).

## Original Pascal trap contract

Reference provenance: Apple's original [Drag Manager reference](https://dev.os9.ca/techpubs/new/Drag_Manager/Functions/Miscellaneou30456789101112.html)
and [Drag Manager programming notes TN1043](https://leopard-adc.pepas.com/technotes/tn/tn1043.html),
cross-checked against the local Retro68 Multiversal `Multiverse.h` declarations.
`_DragDispatch` is `0xABED`, selector in D0, reversed Pascal arguments on the
guest stack with a checked OSErr result slot. Guest pointers are checked offsets;
native callback pointers are never stored in the shared service.

Implemented selectors: NewDrag (5), DisposeDrag (6), AddDragItemFlavor (7),
SetDragItemFlavorData (9), CountDragItems (14), GetDragItemReferenceNumber (15),
CountDragItemFlavors (16), GetFlavorType (17), GetFlavorFlags (18),
GetFlavorDataSize (19), GetFlavorData (20), GetDragItemBounds (21),
SetDragItemBounds (22), GetDragMouse (26), GetDragOrigin (28) and
GetDragModifiers (29). Item references are source-chosen values; enumeration
indices are one-based. Sender-only flags hide flavors from foreign receivers.

Unimplemented selectors fail explicitly. The original install/remove callback
chains, synchronous TrackDrag execution, send/input/drawing procedures,
HFSFlavor/FSSpec file projections, pinned mouse setter, attributes, original
hilite/scroll/zoom calls and Gestalt advertisement still need compatibility
closure. GetDragMouse currently returns the physical point for both physical and
pinned outputs. The original 68k execution layer remains future work; these
marshalling tests do not establish original application execution compatibility.
