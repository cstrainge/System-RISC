# Hardware-accelerated QuickDraw: the interposer and the Rasterise extension

Status: design, no code. This document specifies the layer that lets QuickDraw
operations reach the 3D block, and the contract it places on the compositing
driver that already exists. It is written to be implemented after
`docs/QPU-RESEARCH.md` and alongside `docs/GPU-SUBSYSTEM.md`.

It is deliberately **not** a compositing design. Composition — the HVS, the
present path, the surface the system draws into — is owned by the platform lane
and has a working implementation (`docs/VIDEOCORE-2D-DESIGN.md` §5 and §12;
`drivers/videocore-framebuffer`; `crates/mac-video-interface`). This document
consumes that driver and states what it must provide. Where the existing
contract cannot satisfy this design, §9 records a **named interface gap** with
the reason, so it can be routed as a work item rather than designed around.

---

## 1. What is given, and what is new

These are settled and are not relitigated here:

1. **QuickDraw semantics live in shared software, once.** The hardware layer
   interposes over the software implementation and delegates what it cannot
   accelerate. It is an optimiser, not a second implementation, and it is
   stateless with respect to QuickDraw: software is authoritative; the
   interposer may cache hints only.
2. **The interposer runs in-process; the driver does not.** Interception needs
   the same address space as the QuickDraw implementation, so the interposer is
   linked into System. Hardware primitives cross the driver interface to an
   isolated driver.
3. **Two destination models, and the first is the target.** (a) the accelerator
   writes the same bytes software would have written; (b) it records a display
   list and composites straight from it. (a) lands first; §7 states what must be
   true to move to (b).
4. **Batch boundaries** are port close, a timeout derived from the display's
   frame interval, or process exit, whichever comes first. Port close ends a
   session; the timeout drains a batch within a session without resetting port
   state. A flush lands on an operation boundary, never mid-operation. One flush
   routine serves all three triggers.
5. **No deferred hardware work outlives the memory it references.** Completion
   precedes release on every path, including crash.
6. **Two capabilities, not a tier ladder:** `Rasterise` and `Compose`, each with
   a software default and an optional hardware provider, degrading independently,
   selected once at the platform boundary.
7. **One validated submission path, two front ends** — this interposer and the
   3D API. Submission is admitted behind a kernel-side service entry, because
   BCM2837 has no IOMMU. The kernel does not implement a graphics API; it is the
   boundary where submission is admitted and routed. `docs/GPU-SUBSYSTEM.md` §6
   argues what "admitted" means, and it is **construction**, not validation of a
   supplied list.
8. **Pixel-exact conformance.** QuickDraw is integer, so exactness is attainable
   even through a shader (integer arithmetic in fp32 is exact to 2²⁴). §8
   specifies a corpus that renders identical scenes through each provider and
   compares bytes, not a tolerance.

What is new here:

* the interposition points and the divert/delegate decision;
* the **resolved-span interface** the interposer hands the hardware side;
* transfer-mode capability declaration;
* the three flush triggers and the single flush routine;
* the crash and quarantine path;
* the **contract this layer places on the compositing driver**;
* the pixel-exact corpus.

---

## 2. Where the pieces sit

```
  application
      │ QuickDraw trap (existing ABI)
      ▼
  resident System: QuickDraw dispatch
      │
      ▼
  QuickDraw interposer ──delegate──► QuickDraw software implementation
      │                                 (authority)
      │ Rasterise capability
      ├──► software provider
      │
      ▼
  hardware provider ──GPU request──► kernel GPU service entry
                                          │ admission
                                          ▼
                                    GPU subsystem
                                    objects · contexts · queue · fences · accounting
                                          │
                                          ▼
                                    isolated V3D driver
                                    list construction · registers
                                          │
                                          ▼
                                    V3D at 0x3FC00000
                                          │ writes
                                          ▼
                                    surface ──► compositing driver (platform lane)
                                                VIDO ATTACH / SERVICE / APPLY
                                                ──► display
```

Three boundaries matter:

* **Interposer ↔ software QuickDraw.** A call, in the same address space, with
  a decision to divert or delegate. Cheap, synchronous, and allowed to fail to
  divert at any time.
* **Interposer ↔ hardware provider.** A resolved primitive, not a QuickDraw
  operation. §5 defines it. It crosses into the GPU client library and from
  there to the kernel service entry.
* **GPU ↔ surface.** The accelerator writes into memory the compositing driver
  owns. §9 is the contract for that, and it is the part that does not yet exist
  in the interface.

The interposer never touches a register, never allocates device memory, and
never talks to the driver directly. The hardware provider is a client of the GPU
subsystem, exactly like the 3D front end.

---

## 3. What the interposer intercepts

Interception is at the **resident QuickDraw operation entry**, before the
software implementation runs. It is not at the trap boundary, and it is not in
the application: an application's QuickDraw call arrives as a trap, is decoded
by the resident dispatch, and the interposer is what sits between dispatch and
the software implementation.

Operations the interposer observes:

| Group | Operations |
|---|---|
| Port state | `OpenPort`, `ClosePort`, `SetPort`, `GetPort`, `SetClip`, `GetClip`, `SetOrigin`, `GetOrigin`, `ClipRect`, `SetPortBits`, port colour and pattern fields |
| Fills | `PaintRect`, `FillRect`, `EraseRect`, `InvertRect`, `FrameRect`, `PaintRgn`, `FillRgn`, `EraseRgn`, `InvertRgn`, `FrameRgn`, `PaintOval`, `FillOval`, `FrameOval`, `PaintRoundRect`, `FillRoundRect`, `FrameRoundRect`, `PaintArc`, `FillArc`, `FrameArc`, `PaintPoly`, `FillPoly` |
| Copies | `CopyBits`, `CopyMask`, `ScrollRect` |
| Text | `DrawChar`, `DrawString`, `DrawText`, and the glyph-blit path beneath them |
| Lines | `MoveTo`, `LineTo`, `Line`, `DrawLine` |
| Colour | `RGBForeColor`, `RGBBackColor`, `ForeColor`, `BackColor`, `OpenCPort`, `CloseCPort` |
| Transfer | `PenMode`, `PenPat`, `BackPat`, `PenNormal` |

The names are the classic Toolbox names. This project's QuickDraw surface is the
authority on which of them exist; an operation the system does not implement is
simply absent from the interception table, and its absence is not a gap in the
design.

Everything else is passed through untouched. The list is deliberately wide: the
interposer must *see* every operation that can change the destination or the
state that decides what the next operation writes, even when it will delegate.
Seeing an operation is not the same as handling it.

### 3.1 What "stateless" means concretely

The interposer may hold, per port, only **hints**:

* whether the port's destination surface is currently hardware-resident;
* the last clip and origin it saw, and a flag saying whether they are believed
  current;
* the last pattern, colours and transfer mode it saw;
* the open batch, if any.

Every hint must be reconstructible from QuickDraw state that software owns. The
rules are:

1. **A hint is never authoritative.** If the interposer believes a clip is a
   rectangle and software's clip is a region, the interposer delegates.
2. **A hint miss costs a delegate, never a wrong pixel.** There is no path in
   which a stale hint changes what is drawn.
3. **Port state changes invalidate hints.** `SetClip`, `SetOrigin`,
   `SetPortBits` and a colour or pattern change drop the affected hints.
4. **No QuickDraw state is duplicated.** The interposer must not hold a copy of
   a region, a pattern, or a port record that software could have changed
   behind it. It holds identifiers and scalars.

The reason for the strictness is that QuickDraw state is shared, mutable and
reachable from legacy code. A duplicated copy would eventually diverge, and the
divergence would show as a wrong pixel that no test catches.

---

## 4. Divert or delegate

The default is **delegate**. An operation is diverted only when the hardware
provider has declared that it implements that operation *exactly* for the
current state. The declaration is per transfer mode and per primitive class, not
a single "hardware present" flag.

### 4.1 First landing

| Operation | State required | Why it is safe |
|---|---|---|
| `PaintRect` / `FillRect` / `EraseRect` | Axis-aligned; solid colour; `srcCopy`; clip is a rectangle or empty | A rectangle of one colour; exact by construction. |
| `CopyBits` | `srcCopy`; both surfaces hardware-resident; source and destination rectangles axis-aligned; no mask; clip is a rectangle | A straight copy of a rectangle; exact if the shader copies whole pixels. |
| `ScrollRect` | Same as `CopyBits`, with the direction of the copy chosen so the source is not overwritten before it is read | Same operation as `CopyBits`, with the source and destination in one surface; the ordering rule is software's, and the hardware path must reproduce it. |
| `DrawString` / `DrawText` | Monochrome glyphs from the system font; `srcCopy`/`srcOr` on a known background; integral baseline | A glyph is a 1-bit mask blit; a run is a batch of mask blits against a glyph atlas. Diverted only after the corpus proves it. |
| Pattern fills | 8×8 pattern; `srcCopy`/`srcOr`/`srcXor`/`srcBic` on a known background | The pattern is a texture with the phase derived from the destination coordinate, which is exactly QuickDraw's rule. |

### 4.2 Never in the first landing, and why

| Operation | Reason |
|---|---|
| Any transfer mode other than the declared set | QuickDraw's arithmetic modes are per-pixel boolean/arithmetic operations with exact carry and dithering semantics. A mode is added to the declared set only when the corpus proves it byte-for-byte. |
| Arbitrary region clipping (`RGN`) with holes or a non-rectangular outline | The hardware path would have to reproduce QuickDraw's odd/even span algebra. The resolved-span interface (§5) can express it, but the first landing only accepts a rectangle or a set of disjoint rectangles. |
| `CopyMask` with an arbitrary mask | The mask is a second surface with its own clip and transfer mode; the combination is where QuickDraw's semantics are most subtle. |
| Oval, round-rect, arc and polygon fills | These are scan-converted by QuickDraw's own integer algorithms, including its treatment of the boundary. Reproducing them is a reimplementation, which decision 1 forbids. They delegate until a provider can be shown exact. |
| Text with fractional positioning, or a non-system font | Depends on font machinery and rounding that is not yet specified for the hardware path. |
| Any operation whose destination is not hardware-resident | There is nothing to accelerate. |
| Any operation during a batch whose state has changed since the batch opened | The batch must be closed first; the flush is on an operation boundary. |

The table is a statement about the *first* landing. It is not a claim that the
delegated operations can never be accelerated; it is a claim that they must not
be until the corpus demonstrates exactness.

### 4.3 Transfer-mode capability declaration

The hardware provider declares, at capability-negotiation time:

| Field | Meaning |
|---|---|
| `modes` | The set of QuickDraw transfer modes it implements exactly, each with the pixel formats and pattern/colour combinations it covers. |
| `surfaces` | The surface formats it can write (first landing: linear XRGB32 only). |
| `clip` | Whether it accepts a rectangle, a disjoint rectangle list, or an arbitrary span list. |
| `source` | Whether it accepts a source surface, a solid colour, an 8×8 pattern, or a glyph atlas. |
| `max_span_pixels` | The largest single resolved span it will accept; larger spans are split by the interposer. |

A mode absent from `modes` is delegated, always, without trying it and without
a runtime failure. The declaration is what makes the interposer's decision
cheap and total: there is no case where the hardware path is attempted and
fails.

---

## 5. The resolved-span interface

The interposer hands the hardware side a **resolved primitive**, not a QuickDraw
operation. "Resolved" means every decision QuickDraw would make about *which
pixels* has already been made, by software's own rules, and is expressed as
spans.

### 5.1 Why spans

QuickDraw's clipping reduces to a set of horizontal spans per row: the
intersection of the operation's shape with the clip and the visible surface.
Expressing the primitive as spans has three properties the design needs:

1. **Exactness by construction.** The hardware side cannot invent a pixel
   outside the spans, and cannot miss one inside them, because the spans *are*
   the pixel set.
2. **No region algebra in the hardware path.** The interposer reuses software's
   region intersection; the hardware side never sees a region.
3. **A natural unit for batching.** Spans from consecutive operations can be
   coalesced into rectangles by the hardware side if it chooses, but only
   within the spans it was given.

### 5.2 The primitive

| Field | Content |
|---|---|
| `destination` | Surface handle, device address, width, height, stride in bytes. |
| `spans` | A list of `(y, x_start, x_end)` in destination coordinates, already clipped to the surface. |
| `source` | One of: solid colour (a 32-bit value), a source surface (handle, address, stride, origin), an 8×8 pattern (the 8 rows, as bytes), or a glyph (a 1-bit mask with its own bounds). |
| `phase` | For a pattern or glyph, the `(x, y)` origin used to derive the phase; for a source surface, the source origin. |
| `mode` | The declared transfer mode identifier. |
| `colour` | Foreground and background values, for modes that use them. |
| `flags` | Whether the operation may write outside the spans (always false), whether the destination's ignored byte is written (see §8.3), and whether the spans are ordered top-to-bottom. |

The primitive carries **no** QuickDraw structure: no port, no region, no
`BitMap` record, no transfer-mode procedure pointer. A hardware provider could
be implemented without any knowledge of QuickDraw at all, which is the test of
whether the interface is right.

### 5.3 What the provider may and may not do

May: coalesce adjacent spans on a row into one rectangle; coalesce identical
consecutive rows into a rectangle; reorder spans as long as overlapping writes
are ordered as software would order them; split spans to fit
`max_span_pixels`; drop a span that is empty.

May not: extend a span by one pixel to align it; write a pixel outside the
spans; assume the spans are sorted; assume the destination is opaque; assume
the source and destination do not overlap (for `ScrollRect` they do, and the
ordering rule is part of the operation's semantics).

The last point is the one that most often breaks a hardware blit: a
rectangle-to-rectangle copy in one surface must read before it overwrites, in
the direction QuickDraw chooses. The provider is told the direction explicitly
in `flags` rather than being left to infer it.

---

## 6. Batching and the single flush routine

### 6.1 Triggers

| Trigger | Meaning | Effect on the session |
|---|---|---|
| **Port close** | `ClosePort`, or the port's destination surface is replaced | Ends the session. Queued work is flushed, then the session's objects are released. |
| **Timeout** | No new operation for the batch window | Drains the batch. The session stays open and port state is unchanged. |
| **Process exit** | The owning application ends, normally or by crash | Ends the session and runs the crash path (§6.4). |

### 6.2 The batch window

The window is derived from the display's frame interval, not chosen as a
constant: `window = clamp(frame_interval / 16, 500 µs, 2 ms)`.

At 60 Hz this is 1.04 ms, which is the "millisecond timeout" in the brief; at
50 Hz it is 1.25 ms; at 30 Hz it is clamped to 2 ms. The derivation matters
because a fixed millisecond is wrong at both ends: at 30 Hz a 1 ms window would
flush several times per frame and lose batching, and at 120 Hz it would hold a
batch past the frame it belongs to.

The window is measured from the **last operation admitted to the batch**, not
from the batch's start, so a stream of operations keeps extending it. A batch is
also closed when it reaches a size bound (spans, bytes of resolved primitives,
or destination area), so a single operation cannot create an unbounded batch.

### 6.3 One routine

There is exactly one flush routine, with one parameter: whether the session
ends. It performs, in order:

1. If the batch is empty, return.
2. Submit the batch's resolved primitives to the hardware provider as one GPU
   request (one submission, so one completion).
3. Record a completion obligation against every destination surface the batch
   names.
4. If the session is ending, wait for the completion (or take the crash path),
   then release the surfaces and the GPU objects.

The timeout trigger calls it with "session continues"; port close and process
exit call it with "session ends". Nothing else submits. In particular, the
interposer must not submit a partial batch to make room: if the size bound is
reached, that is a flush on an operation boundary, which is the same routine.

### 6.4 The crash path

The rule is decision 5: **no deferred hardware work outlives the memory it
references**. On process exit, normal or not:

1. Stop admitting operations to the batch.
2. Flush the batch with "session ends", unless the process died inside an
   operation, in which case the partially built batch is discarded rather than
   submitted — a half-built primitive is not a primitive.
3. Ask the GPU subsystem to cancel and then wait for the context's outstanding
   jobs, with a bound.
4. If completion is established, release the surfaces and objects.
5. If completion is **not** established (the GPU is hung), reset the block, then
   release. If the reset fails, **quarantine** the objects: mark them
   unreclaimable, charge them to the device rather than to the dead context, and
   report a degraded device. Freeing memory a possibly-in-flight job references
   is the one thing that must never happen.

Quarantine is not a leak by accident; it is a deliberate, accounted state with a
diagnostic. A later successful reset may reclaim it.

---

## 7. The two destination models

### 7.1 Model (a): the accelerator writes the same bytes

The hardware provider renders into the surface the compositing driver already
scans out (or already composites), using the same stride and the same pixel
format. Software and hardware are then interchangeable at the byte level: the
same scene, drawn by either, produces the same buffer, and the present path does
not know which one drew it.

This is the first landing because it needs **no new interface from the
compositor** beyond a device address and lifetime (and even those are gaps in
the current interface, §9). It also makes the corpus meaningful: the comparison
is byte equality of the surface.

What it requires to be true:

* **the destination is the drawable the video driver owns**, not an arbitrary
  caller-supplied pixel buffer. This is not automatic: the QuickDraw destination
  arrives as a bare pixel pointer with no allocation identity (G7), so the
  interposer must ask and delegate when the answer is "not a GPU-writable
  surface";
* the surface is linear, with stride = width × 4, in XRGB32;
* the GPU can write it at its device address, with cache maintenance around the
  write;
* the surface is not released or reallocated while the job references it;
* the compositor's present does not run concurrently with the GPU's write to the
  same rectangle (or does so harmlessly, because the write is atomic per pixel
  and the present copies whole pixels).

### 7.2 Model (b): record a display list and composite from it

The provider records what was drawn as surfaces and operations, and the
compositor composites from those surfaces directly — a window that has not
changed is a plane, not a repaint.

This is the end state because it removes the copy and the repaint, which is
where the real saving is (`docs/VIDEOCORE-2D-DESIGN.md` §11.1). It is not the
first landing because it requires the compositor to accept a *scene*, not a
surface.

### 7.3 What must be true to move from (a) to (b)

1. **The compositor accepts multiple surfaces with z-order.** Today `VIDO` v1
   hands System one surface and takes damage against it. A scene is a different
   contract.
2. **Per-surface publication is atomic and acknowledged.** Publishing a scene
   must not tear, and the producer must learn when the compositor has stopped
   reading a surface.
3. **The GPU can render into an off-screen surface that the compositor can then
   scan out.** That needs the surface's device address and format to be part of
   the handover, which is gap G1 in §9.
4. **Damage becomes scene damage.** A plane move is not a pixel copy; a content
   change is bounded to a surface. The two must be separate in the contract, as
   they already are in the resident damage model.
5. **Formats and tiling are negotiated.** Model (a) can assume linear XRGB32
   because that is what exists. Model (b) will want at least one tiled format,
   because a plane fetched by the HVS is more efficient tiled, and then the
   GPU's render target and the compositor's source are the same tiled surface.

None of those is this document's to design. They are the conditions under which
model (b) becomes possible, and they are stated so that the migration is a
decision rather than a surprise.

### 7.4 The HVS composition path, as it stands

The first hardware win on this platform is composition by the existing driver,
and it needs no shader compiler: the HVS fetches planes, scales and blends them
at scan-out, and a plane move costs a display-list update rather than a repaint.
That work belongs to the platform lane. This design's contribution is to make
the *drawing* into those surfaces optionally hardware-accelerated, so that when
the compositor gains planes, the content of the planes can be produced by either
provider.

The contract in §9 is written to survive both states: it asks for a device
address and a lifetime, not for planes. A compositor that later grows planes
satisfies the same contract with more capability.

---

## 8. Pixel-exact conformance

### 8.1 The requirement

**Byte equality of the destination surface, with no tolerance.** After each
operation, and after each batch, the buffer produced by the hardware provider
must equal, byte for byte, the buffer produced by the software provider from the
same initial contents.

Byte equality, not "visually identical", and not a per-pixel tolerance. QuickDraw
is integer; a tolerance would hide exactly the off-by-one errors that make a
hardware path untrustworthy.

### 8.2 Exactness includes what is *not* written

QuickDraw does not write every pixel of its bounding box: a clipped fill writes
only the visible part, a mask blit leaves masked-out pixels untouched, and an
XOR leaves the destination's untouched neighbours alone. The corpus therefore
initialises the destination with a **non-uniform sentinel pattern** (not zeros,
not a solid colour) before each operation, so that a hardware path that writes
one pixel too many is detected. Comparing against a zeroed buffer would hide it.

### 8.3 The ignored byte

The surface is XRGB32: one byte is not colour. Whether software writes a
constant there, or leaves it as it found it, decides what byte equality means.
This is a **named open question** (§10, Q1). Until it is answered, the corpus
compares all four bytes and the provider is told explicitly, per primitive,
whether it writes the ignored byte.

### 8.4 Integer exactness through a shader

The QPU has exact integer add, subtract, shifts, min/max, bitwise and
count-leading-zeros; it has no integer multiply (§7.5 of `docs/QPU-RESEARCH.md`).
A QuickDraw shader therefore:

* uses integer operations only for pixel arithmetic, coordinates and phases;
* never uses `ftoi`/`itof` to convert a coordinate, because the conversion is
  where a rounding mode would enter;
* may use `fmul` only where the operands and the result are integers below 2²⁴,
  where fp32 is exact, and the corpus must cover that case;
* synthesises any 32-bit integer multiply from `imul24` plus correction, or
  avoids it (a pattern phase, for example, is a shift and a mask, not a
  multiply).

### 8.5 The corpus

Each case is a scene: an initial destination, a sequence of operations, and the
expected bytes. Each scene is rendered twice — once by the software provider,
once by the hardware provider — and compared byte for byte. The software result
is the oracle; it is not a hand-written expectation, which would encode the same
misunderstanding twice.

| Group | Cases |
|---|---|
| Rect fills | 1×1, 1×N, N×1, full surface, a rectangle exactly on each edge, one pixel outside each edge, a rectangle spanning the whole surface plus overflow on each side, zero-width and zero-height (no-op) |
| Clip | Rectangular clip smaller than, equal to, and larger than the operation; clip entirely outside; clip sharing one edge; two disjoint rectangles as the clip; a clip that is a region with a hole (must delegate) |
| Patterns | All eight phases of an 8×8 pattern; a pattern whose rows are identical; a pattern with a single set pixel; pattern fill clipped on each edge |
| Copies | Source and destination disjoint; adjacent; overlapping in the copy direction; overlapping against it; source partly off-surface on each edge; destination partly off-surface; 1-pixel-wide and 1-pixel-tall; copy onto itself |
| Masks and text | Single glyph; a run with kerning; glyph clipped by the baseline; glyph at the right and bottom edges; text over a pattern; text in each declared transfer mode |
| Lines | All eight octants; horizontal and vertical; one-pixel lines; endpoints exactly on a clip boundary; lines clipped at both ends |
| Transfer modes | Each declared mode over each background in the corpus; a mode not declared must delegate and still match |
| Geometry extremes | Coordinates at the 16-bit limit; a rectangle whose right edge is at the surface width; a stride that is not a multiple of 16 bytes; a surface whose width is 1 |
| Batches | Several operations in one batch; a batch split by the size bound at every possible operation index; a batch ended by the timeout; a batch ended by port close |
| Concurrency | A copy whose source is being written by an earlier operation in the same batch (ordering must match software's) |

### 8.6 What a mismatch means

A mismatch is a **defect in the provider**, not a reason to widen the
tolerance. The response is to remove the operation from the provider's declared
capability set until it is fixed, which automatically delegates it. That keeps
the system correct while the provider is wrong, and it is the same refusal
discipline the device classes use.

---

## 9. The contract this design places on the compositing driver

This section is the interface requirement, stated so the platform lane can
implement against it. It is not a redesign: each item is the smallest thing this
layer needs, expressed in terms of the existing `VIDO` v1 contract.

The existing contract, for reference: `ATTACH` returns a `Surface` with
`generation`, `allocation`, `address`, `capacity` and `preferred`; `SERVICE`
takes a `Damage` rectangle and returns a `Poll`; `APPLY`/`COMMIT`/`ABORT` change
mode; `RELEASE` ends the surface's life. The surface is XRGB32 with
stride = width × 4.

### 9.1 Requirements

| Id | Requirement | Why this design needs it |
|---|---|---|
| **R1** | The surface handover must expose the drawable allocation's **device (bus) address**, not only the CPU address. | The GPU cannot name a CPU address. It is a bus master with no MMU. The driver already receives this address from the Device service and drops it (G1). |
| **R2** | The allocation must be **GPU-accessible**: either coherent by construction, or with a documented cache-maintenance handover (`clean before device reads`, `invalidate before CPU reads`). | A cached surface written by the GPU is read as stale by the CPU, and vice versa. The resident DMA module already provides coherent pinned storage with a contiguous bus alias; the video driver's surface is not allocated from it (G2). |
| **R3** | The surface must stay allocated and **pinned until GPU work referencing it has completed**, with a completion signal the GPU subsystem can observe. | Decision 5. Today's `RELEASE` drains after System has switched *CPU views*, which is not the same as "the GPU has finished writing". |
| **R4** | A present must be able to be told that the pixels were produced by the GPU, so it can order cache maintenance and (later) flips. | Without it, `SERVICE` can copy a rectangle before the GPU's write is visible. |
| **R5** | Damage should be expressible as a **rectangle list**, not one rectangle. | Spans and multi-rectangle batches produce several disjoint damaged regions; the bounding box is correct but copies more than necessary. Correctness first: the bounding box is acceptable in the first landing. |
| **R6** | A mode change must **notify** the GPU subsystem that the surface is being replaced, before the old one is released. | A mode change reallocates; a job referencing the old allocation must be drained first. |
| **R7** | The surface's byte layout must remain **linear XRGB32, stride = width × 4**, or the driver must declare its layout. | Byte-exact conformance compares whole pixels; a tiled or padded surface changes what "the same bytes" means. |
| **R8** | There must be a way to ask whether a given destination is a surface the GPU may write, and to be told "no" with a reason. | The QuickDraw destination is a caller-supplied pixel buffer with no allocation identity (G7). The interposer's divert decision depends on this answer. |

### 9.2 Named interface gaps

These are places where the existing contract, as read in
`crates/mac-video-interface` and `drivers/videocore-framebuffer`, does **not**
satisfy a requirement. They are recorded as work items, not designed around.

| Gap | Observation | Consequence | Suggested routing |
|---|---|---|---|
| **G1 — the surface drops a device address it already has** | The Device DMA service returns both a CPU address and a device address (`mac_device_interface::Allocation`). The driver's `allocate_deferred` receives `buffer.device_address` and returns a struct carrying only `cpu_address`; `free` then reconstructs the interface record with `device_address: 0`. `mac_video_interface::Surface` therefore carries a CPU address only. | The GPU cannot name the surface. | Three lines in the driver, not a missing capability: carry the device address through the driver's own `Allocation` and into the surface handover. |
| **G2 — cache maintenance is private to the driver** | The driver's machine adapter owns `clean`/`invalidate` and uses them for the firmware buffer. The resident DMA module (`system/src/machine/dma.rs`) already provides pinned, charged, coherent resident DMA storage with a contiguous bus alias via `machine::paging::bus_address`, so the *mechanism* exists — but there is no operation by which the video driver's surface can be handed to another device with a documented maintenance handover. | A GPU-written surface may be copied by a stale CPU cache line. | Either allocate the drawable from the resident DMA module (which is coherent by contract) or expose an explicit maintenance operation on the surface. |
| **G3 — no device-side in-flight hold on the surface** | `RELEASE` drains CPU views: the Device service refuses a free while System holds a CPU pin, which proves no *CPU* view remains. The resident DMA buffer's own comment says its storage is retained "until GPU completion/reset and all CPU view pins have drained" — so the rule exists for DMA buffers. The video driver's surface is a Device allocation without that device-side hold. | Freeing a surface under an in-flight job is exactly decision 5's forbidden case. | Add a device hold reference the GPU subsystem takes and drops on completion; `RELEASE` waits for holds to reach zero or reports busy. |
| **G4 — no producer identity on present** | `SERVICE(damage)` has no field saying who wrote the pixels or when. | The present cannot order itself after GPU completion. | Add a producer/ordering token, or require the caller to have waited on the fence before presenting (simplest, and acceptable for the first landing). |
| **G5 — single-rectangle damage** | `Damage` is one rectangle. | Multi-region batches present a bounding box. Correct but wasteful. | A rectangle list in a later revision. Not blocking. |
| **G6 — mode change replaces the surface silently** | `APPLY`/`COMMIT` allocate a new surface; there is no notification to another device that the old one is going away. | A job in flight against the old allocation. | Notification hook, or `RELEASE` already waits for device holds (G3) and the GPU subsystem is told the surface identity changed. |
| **G7 — the QuickDraw destination is not the scanout surface** | The QuickDraw drawing destination arrives as `mac_abi::Surface`: a bare `*mut u32` with width, height, origin and clip. It is the caller's own pixel buffer, supplied as a wire buffer whose length must equal `width × height × 4`; the only bound is that the pixel count does not exceed the display's (`system/src/display.rs::pixels()`), which is a size check, not an identity. Nothing in that record says which allocation the pointer belongs to, whether it is GPU-addressable, or how long it lives. | The interposer cannot assume the destination is the compositing driver's surface. It may be an application's off-screen buffer, which the GPU has no way to name. A design that assumed otherwise would render into the wrong memory. | The interposer must ask whether the destination pointer is backed by a GPU-registered allocation and **delegate when it is not**. Registering an arbitrary application buffer for GPU use is a separate facility that does not exist today; the first landing should divert only into the drawable the video driver owns. |

**G1, G3 and G7 are blocking.** The first landing cannot render into the surface
without G1 and G3, and it cannot decide *which* surface to render into without
G7. G2 is blocking for correctness unless the allocation is coherent. G4, G5 and
G6 have acceptable interim answers (wait before present; bounding box;
hold-based release) and should be recorded rather than rushed.

---

## 10. What is not established

1. **Q1 — the ignored byte.** What does software write in the X byte of an
   XRGB32 surface? Constant, or preserved? Byte equality depends on the answer.
   Resolution: read the software path; until then, the provider is told per
   primitive and the corpus compares all four bytes.
2. **Q2 — the actual cost of a diverted fill.** No measurement exists on this
   board. The design assumes a rectangle fill through the 3D pipeline can beat a
   cache-friendly CPU loop for large areas and will lose for small ones; the
   threshold is unknown and must be measured, not guessed.
3. **Q3 — whether the batch window is the right constant.** `frame_interval/16`
   is derived, not measured. It must be tuned against the Cursor Test.
4. **Q4 — glyph atlas strategy.** Whether text is a per-run texture upload or a
   resident atlas is a performance decision that depends on Q2.
5. **Q5 — region clipping.** Whether arbitrary `RGN` clips can be expressed as
   spans cheaply enough to be worth it, or should always delegate.
6. **Q6 — the transfer modes beyond `srcCopy`.** Each must be proved by the
   corpus before being declared; none is established.
7. **Q7 — overlap ordering for `ScrollRect` in one surface.** The rule is
   QuickDraw's, and reproducing it through a shader that may read and write the
   same surface needs a board test (a tile-buffer read of the destination is the
   likely mechanism).
8. **Q8 — what QEMU can and cannot reach.** Not "nothing": the block is an
   unimplemented placeholder returning zero at the silicon's own address, so the
   hardware provider's *refusal* and the fallback to the software provider are
   verifiable, as are the interposer's decisions, the span algebra and the
   corpus's software side on the host. The HVS is not modelled at all. The
   accelerator, the surface handover and every pixel comparison are board-only —
   `docs/QPU-RESEARCH.md` §3.5.

---

## 11. Board runs this design needs

1. **Bring-up:** clock enable, reset release, `V3D_IDENT0` reads `0x02443356`,
   `V3D_IDENT1` gives the slice/QPU/TMU/VPM/semaphore counts.
2. **One rectangle fill** into the compositing driver's surface at its device
   address, presented, and compared byte-for-byte with the software fill.
3. **One `srcCopy`** of a rectangle, including the overlapping case in both
   directions.
4. **One pattern fill**, all eight phases.
5. **One text run**, compared against the software run.
6. **A batch** of mixed operations, ended by each of the three triggers, with
   the completion observed before release.
7. **A deliberate hang**, to exercise the timeout, the reset, and the
   quarantine path — in a private guest, never against the live desktop.
8. **A cache experiment:** write the surface from the CPU, present without
   cleaning, and observe whether stale pixels appear. This establishes whether
   G2 is real on this board or whether the allocation is already uncached.

What QEMU can and cannot reach is stated in `docs/QPU-RESEARCH.md` §3.5. In
short: the interposer's divert/delegate decisions, the span algebra, the batch
triggers, the crash path's bookkeeping and the software side of the corpus are
all exercisable on the host or under QEMU. The hardware provider, the surface
handover and the accelerator are not — under QEMU the V3D is an unimplemented
placeholder that returns zero, so the driver refuses to attach. That refusal,
and the fallback to the software provider it must produce, is the one
hardware-related behaviour QEMU does verify, and it should be a test rather than
an accident.

---

## 12. Decisions a reviewer must make

Identifiers are global and defined once, in `docs/QPU-RESEARCH.md` §16.1. This
document originates these:

| Id | Decision | Argued in |
|---|---|---|
| **D15** | The first-landing divert set; everything else delegates. | §4.1, §4.2 |
| **D16** | The interposer is resident and in-process; the hardware provider is a GPU client. | §2 |
| **D17** | `Rasterise` is a capability with a software default. | §1, §4.3 |
| **D18** | The resolved-span interface is the boundary. | §5 |
| **D19** | The batch window derivation, subject to tuning. | §6.2 |
| **D20** | The ignored-byte policy, decided before the corpus is written. | §8.3, §10 Q1 |
| **D21** | Model (b) is not scheduled. | §7 |

D1, D2 and D4 (construction, the no-addressing subset, and the linear XRGB32
target) are argued elsewhere and are prerequisites here. The routing this
document needs is G1, G2, G3 and G7 in §9.2 — recorded as gaps, not decided
here.
