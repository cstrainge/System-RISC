# Drag Manager implementation and compatibility

System owns one generation-checked drag manager for native and original trap
callers. The SDK exposes a sole RAII source owner, owned tracking responses,
owned drop-only take-once data and typed terminal outcomes. Original counted-byte data
reads remain repeatable; they are not native takes. Source/receiver thread and
owner checks are separate from handle generation checks. Definitions and feedback
never execute application callbacks in another application's context.

## Registered lifetimes and tracking expiry

DRAG/DRST own session payloads and their per-domain backing. DRCV/DRVS own
receiver registrations and backing, including receivers created with windows.
The generic owner walk replaces the separate Drag Manager EXIT call. Source
release wakes a pending receiver take, and receiver release rejects the drop
and wakes its surviving source. Ordinary IDs are revoked before cleanup, while
retained guards keep the payload and provider pins alive through destruction.
The shared projected store preserves the current 32-bit namespaces and their
existing global wire limits (32 sessions, 64 receivers). It rejects namespaces
without boot-wide generation history, including at reconstruction boundaries.
Native and original traps continue to validate one underlying registered object.

A tracking event is a revocable view. `respond` returns
`TrackingResponse::Applied` or `TrackingResponse::Expired` in the same trap;
there is no separate validity query. Expired positions and responses after a
committed drop cannot overwrite negotiation or trigger a false access-denial
failure. Finder stages a replacement application target/document batch and keeps
the previous pins until the response is applied. The deterministic regression
holds a tracking view across mouse-up, responds late, and verifies the original
drop remains readable and completes. Ordinary wrong-thread/foreign/request
refusals remain typed errors, with informative Display and nested causes.
Filesystem admission happens after position/phase validation; a missing target
returns TargetUnavailable with its exact cause and releases the response guard.
It cannot be misclassified as normal expiry or leave mouse-up waiting forever.

## Current verified slice

`mac-toolbox/tests/drag.rs` exercises real resident services through the SDK and
the original Pascal A-line gate: lazy same-app producers, take-once, duplicate
pending takes, cancellation wakes, source ownership, WrongThread, stale-reference
slot reuse, one-based original enumeration and partial repeatable flavor reads.
The decoded event and its tracking/drop capabilities are intentionally neither
`Copy` nor `Clone`. Ignoring a tracking response rejects that position; ignoring
a drop or abandoning its receive future rejects the drop, including before its
first poll. Cleanup validates receiver ownership but can run on another app
thread without granting data access there. Queue peeks return non-owning
`EventSnapshot` metadata and never obtain/release a protocol capability. Finder
additionally scopes its source owner to dispatch success, so a failed file
operation disposes it before opening an alert. Modal UI cannot consume lifecycle
cleanup. Native physical acceptance rejects two unsupported forced copies before
drop, repeats two lawful moves that fail on a destination name collision with
alerts, then performs successful single/group drags and Escape in the same run.

Accepting responses declare `DropActions::{Copy, Move, CopyOrMove}`. System
resolves the action against current modifiers during tracking and again at
release. A receiver's missing action never silently coerces the user's request.
Changing to an unsupported modifier removes the highlight before the application
handles its next offer. Answered rejection is distinct from an outstanding
response, so mouse-up after rejection completes without waiting forever.
Finder also ends its double-click sequence after any actual drag, including a
rejected or cancelled one; an immediate retry cannot launch the dragged app.

Folder/Trash intent also checks the resident File Manager's operation readiness.
The current atomic catalog move preserves volume identity: forced cross-volume
moves and provider mounts without that operation reject during negotiation. The
query reads mount state only; tests assert zero device I/O, and pointer-policy
decisions use source-volume metadata frozen at begin rather than walking items.
The writable host provider implements metadata, rename, move, remove and stepped
transactional groups. Native filesystem workflow acceptance copies applications
with both forks, launches/quits them from the host mount, moves back to native
storage, opens external documents by application drop, applies modifiers at drop
and ejects writable/read-only volumes. Read-only drivers retain the complete read
contract while rejecting storage mutation.

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

The memory-model stage-6 gate carries the earlier Res Edit exhaustion finding:
repeated viewer changes exhausted available memory while configuring/drawing a
scroll box. This drag slice does not qualify the full Res Edit suite. The planned
page-backed heaps/resource accounting must pass that suite without a machine halt;
increasing the static heap is not a substitute for that acceptance.

Native offers carry their original parent identity. A group move validates all
expected parents and publishes placement/catalog changes in one transaction.
Repositioning does not re-query each source file at drop. Closed-folder/volume
drops preserve relative layout in destination coordinates. There is no app-local
pointer tracking or direct source deletion on a successful receiver outcome.

Finder cross-volume copying, forced copies, transfer windows/rollback, desktop
placement and volume eject are exercised by native filesystem workflow tests.
The resident policy samples modifiers at release: Command forces move, Option
copy, Command wins, Trash always move, read-only sources are copy-only and
read-only destinations reject. Text-field drag clients, the general application
executor and real guest std/Tokio support remain separate work; see
[ASYNC-SDK.md](ASYNC-SDK.md).

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
