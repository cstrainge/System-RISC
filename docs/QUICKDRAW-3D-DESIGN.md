# The 3D front end: API, admission, lifetime and failure

Status: design, no code. This document specifies the 3D API this system exposes,
how a call reaches the GPU, what a submitted job may reference, and what happens
when it goes wrong. It is the second of the two front ends over the single
submission path; the first is the QuickDraw interposer
(`docs/QUICKDRAW-HW-DESIGN.md`). The kernel-side machinery it calls is
`docs/GPU-SUBSYSTEM.md`; the hardware facts are `docs/QPU-RESEARCH.md`.

The hard constraint is stated once and then used throughout: **BCM2837 has no
IOMMU.** The GPU is a bus master that reads and writes physical addresses, and a
wrong physical address is simply a wrong address. `docs/GPU-SUBSYSTEM.md` §6
argues that the response is not to validate arbitrary control lists but to never
accept one: the application submits structured parameters and the driver
constructs the list. This document is the API consequence of that decision.

---

## 1. Scope: what the API is, and what it is not

### 1.1 The case against "OpenGL"

An OpenGL (or GLES) implementation is the wrong target, for reasons that are
specific to this system rather than a matter of taste:

* **The pipeline is not there.** VC4 has no fixed-function blender: blending is
  performed by the shader reading the tile buffer, writing the result, and
  letting the hardware store it. Depth test, culling and coverage are fixed
  function; blending, texturing arithmetic and colour conversion are shader
  code. A GLES state machine would therefore have to *compile* its blend state
  into shader variants — which is fine, but it means the "API" is really a
  compiler front end, and pretending otherwise hides the work.
* **The resource model does not fit.** GLES assumes a driver that can move
  objects, allocate lazily, and rely on an MMU for safety. This system has to
  name physical addresses and account for every byte.
* **The validation surface would be enormous.** Every GLES entry point is a
  parameter-validation problem, and the ones that matter here (buffer sizes,
  formats, offsets) are exactly the ones the construction model already
  handles — but without a general state machine to get wrong.
* **Nothing in the system needs it.** The two consumers are the QuickDraw
  interposer (rectangles, blits, patterns, glyphs) and a small set of 3D scenes.
  Neither needs GLES.

### 1.2 The API that is proposed

A **small immediate-mode API over retained objects**:

* *Retained*: buffers, textures, programs and pipelines are objects with
  handles, created once and reused.
* *Immediate*: a draw call names everything it needs — program, buffers,
  uniforms, target, viewport, scissor, pipeline — and returns. There is no
  context state machine to get out of sync, no bind/unbind ordering, and no
  hidden state that a validation pass has to model.

This shape is chosen because it is the one that can be **completely checked
before anything reaches the GPU**: every field of a draw is a handle or an
integer with a bound, and the set of checks is finite and enumerable (§4). A
state machine would multiply the checks by the number of reachable states.

### 1.3 The surface

| Operation | Inputs | Result |
|---|---|---|
| `device_info` | — | Slice, QPU, TMU, VPM and semaphore counts; format support; limits. Read once, at start-up. |
| `create_context` | Priority hint | Context handle. |
| `destroy_context` | Context | Releases every object the context owns, after its work completes. |
| `create_buffer` | Size, usage flags | Buffer handle, CPU mapping, device address (not exposed to the application). |
| `destroy_buffer` | Buffer | — |
| `map_buffer` / `unmap_buffer` | Buffer, range | CPU pointer. Refused while the GPU may write the range. |
| `create_texture` | Width, height, format, tiling | Texture handle. First landing: linear, 2D, no mips, formats RGBA8888 and RGB565 and luminance/alpha. |
| `destroy_texture` | Texture | — |
| `create_program` | Compiled shader binary and metadata from the compiler | Program handle, or a refusal naming the failing instruction. |
| `destroy_program` | Program | — |
| `draw` | Program, pipeline, target, viewport, scissor, vertex buffer + offset + count, optional index buffer, uniform block | A fence. |
| `clear` | Target, colour, depth | A fence. |
| `present_target` | Target surface, damage | Hands the surface to the compositing driver's contract. |
| `fence_wait` | Fence, timeout | Completed, or timed out. |
| `readback` | Buffer range, after a fence | Bytes, with cache invalidated. |

Deliberately absent from the first landing: mipmaps, cube maps, 3D textures,
geometry/tessellation shaders, multiple render targets, compute shaders, queries,
conditional rendering, transform feedback, instancing, and any form of
user-supplied control list. Each is a later decision, and each has a cost that
is stated where it comes up.

### 1.4 The pipeline state

`pipeline` is a small value, not an object with mutable state:

| Field | Values | Where it is implemented |
|---|---|---|
| Depth test | never / less / equal / lequal / greater / gequal / always | Fixed function (configuration bits) |
| Depth write | on / off | Fixed function |
| Cull | none / front / back | Fixed function |
| Front face | CW / CCW | Fixed function |
| Blend | none, or source-over with a declared factor set | **Shader** (see below) |
| Colour write mask | per channel | Shader or store configuration |
| Render target format | RGBA8888 / RGB565 | Fixed function and shader packing |

Blending is the interesting entry. Because there is no blender, the pipeline
value is part of the **program's key**: the compiler emits a variant of the
fragment shader that reads the tile buffer, computes the blend, and writes the
result. The API therefore requires the program to have been compiled for the
pipeline's blend mode, and refuses a draw whose pipeline the program was not
compiled for. That refusal is by name (§5), not a silent approximation.

---

## 2. How a call reaches the GPU

```
application
   │  (Rust API, no_std)
   ▼
GPU client library ── handles, no device addresses
   │  class interface (GPUX v1): one request record, one reply
   ▼
kernel service entry ── admission: ownership, bounds, liveness, budget
   │  validated, normalised job description
   ▼
GPU subsystem ── contexts, objects, queue, fences, accounting, policy
   │  job description
   ▼
V3D driver ── constructs the control lists, programs the registers
   │  addresses, list addresses, interrupt
   ▼
V3D
```

Two properties of this path are load-bearing:

* **The client library never holds a device address.** It holds handles. The
  address is resolved by the subsystem, in the kernel, from the handle. An
  application cannot learn, guess or forge a physical address, because it is
  never in its possession.
* **The driver never parses application data as instructions.** It reads a
  fixed-layout job description, and it writes a control list from a template.
  The only bytes of application origin that reach the GPU are vertex/index data,
  uniform *values*, and shader machine code — and the last is checked against
  the ISA (§5.2).

### 2.1 Why not call the driver directly

The driver is an isolated module. An application calling it directly would need
the driver's interface to accept application-supplied structure, which is the
thing this design refuses. The resident subsystem is also where the objects,
contexts, accounting and failure policy live; a direct call would bypass all of
them.

### 2.2 The shared contract with the compiler

The compiler and the driver must agree on the **shader ABI**, and it is a
contract with three parts:

| Part | What the compiler reports | What the driver does with it |
|---|---|---|
| Attribute layout | For each vertex attribute: the VPM offset, the size in bytes, and which attribute slot it occupies | Writes the attribute records and the shader record's attribute fields |
| Varying layout | For each varying: the VPM offset in the vertex shader and in the fragment shader, and the count | Writes the shader record's varying count and the VPM offsets |
| Uniform layout | The ordered list of uniform slots the shader reads, with their types and sizes, and the texture descriptor slots | Writes the uniform stream in that order, and the texture descriptors at the declared slots |

The compiler therefore emits **machine code plus a description**, and the driver
consumes the description. The driver never inspects the machine code, and the
compiler never emits an address. This is the mechanism by which "the API is one
the compiler and driver must both understand" is made concrete rather than
aspirational.

---

## 3. What a submitted job may reference

Because there is no IOMMU, the set of things a job can name is the security
boundary. It is enumerated here, and it is closed: anything not on this list is
not expressible.

| May be referenced | How it is bounded |
|---|---|
| A buffer owned by the submitting context | The driver resolves the handle; the offset and length are checked against the object's size |
| A texture owned by the submitting context | Same, plus format, tiling and dimensions are checked against the object's allocation |
| A render target owned by the submitting context, or the compositing driver's surface | The target is an object the subsystem created; its size, format and stride are known |
| A program owned by the submitting context | Its machine code passed the ISA check at creation; its metadata is a fixed-layout record |
| Uniform *values* | Scalar values copied into a driver-built uniform stream |
| Vertex and index data | Inside a buffer the context owns; the index range is checked against the buffer and against the vertex count |
| The driver's own control-list and shader-record storage | Allocated by the driver, never by the application |
| Binner tile state and tile alloc | Allocated by the subsystem from its pool, one slot per job |

| May **not** be referenced | Why |
|---|---|
| A raw physical address supplied by the application | The API has no such parameter. |
| A buffer owned by another context | Ownership is checked; a foreign handle is refused. |
| A released handle, or a handle from a previous generation | Generation-tagged handles; stale is refused, not reinterpreted. |
| A control list, in any form | The API does not accept one. |
| A shader whose instruction stream was not checked | Refused at `create_program`. |
| A texture descriptor, or any other GPU-visible structure | Constructed by the driver from object metadata. |
| An address computed by the shader | The shader is restricted to an ISA subset with no addressing capability (§3.1). |
| A second context's semaphores or fences | Owned by the subsystem and allocated per context. |

### 3.1 The shader subset is what closes the boundary

An earlier form of this argument said "a QPU instruction cannot name an
address". That is **wrong**, and the correction matters. Linux's shader
validator states the real threat in its own file comment: without an IOMMU, a
user who can execute shaders could overwrite system memory through the **VPM
write-address register in general-purpose DMA mode**, or read memory it should
not by reading it **as a texture, as uniform data, or through a direct-addressed
TMU lookup**. Those are three genuine routes by which a shader can reach memory,
and any design that claims otherwise is unsound.

What closes them is not a property of the hardware but a restriction on what the
compiler may emit, enforced by the kernel's check:

| Route | How it is closed |
|---|---|
| VPM general-purpose DMA | The compiler writes the VPM configuration and address registers only with **immediate constants** whose mode selects register-file-to-VPM transfer. The checker rejects any program that writes them with a computed value, and verifies the immediate against the value the driver independently computes. |
| Direct-addressed TMU lookup | Direct addressing is declared in the **shader record's sample metadata**, which the driver writes. The driver refuses a program whose metadata declares it. |
| Uniform data reads | The uniform stream is driver-built from handles; the shader reads only what the driver wrote. |

So the claim that survives is the narrower, checkable one: **the program is in a
subset of the ISA that has no way to name an address, and the kernel proves the
subset was used.** The proof is structural — immediates for the VPM
configuration, metadata for the TMU, driver-built uniforms — rather than a
dataflow analysis over arbitrary shader code. That is what makes it reviewable.

Two consequences follow:

* **The compiler's output is part of the security boundary**, not just a
  performance artefact. `docs/SHADER-COMPILER-DESIGN.md` §4.10 makes the subset
  a hard rule of the backend, and the capability record declares it.
* **The residual risk is a hole in the subset check.** If there is a third route
  to memory, or a way to write the VPM configuration with a value the checker
  believes is an immediate but is not, the result is memory corruption, not a
  wrong picture. That is stated plainly in `docs/GPU-SUBSYSTEM.md` §6.4 and is
  the strongest reason to keep the accepted subset small.

What remains true and useful: **everything that carries an address is
constructed.** The control list, the shader records, the uniforms and the
texture descriptors are all built by the driver from handles and bounds, and the
application contributes no structure. The residual risk is therefore a bug in
the template, in the subset check, or in the compiler's metadata — a much
smaller and more reviewable surface than an arbitrary program, and stated as
such rather than claimed to be zero.

### 3.2 What this costs

Plainly: **an application cannot submit a hand-written control list or a raw QPU
program.** That forecloses:

* GPGPU from an ordinary application — no custom compute kernels, no
  hand-scheduled QPU assembly, no third-party shader toolchain that emits
  hardware code;
* "just this once" workarounds for a primitive the API does not express;
* any technique that depends on the application controlling the exact order of
  GPU operations beyond what fences express.

For this system's stated purpose — a desktop, QuickDraw acceleration and a small
3D API — that is acceptable, and it is the price of running without an IOMMU on
a bus master. It is recorded as decision D1 in §8.

If raw compute becomes a requirement, the smallest trusted path is **not** to
open the boundary. It is:

1. a *privileged* module (kernel-signed, loaded from the blessed System
   directory, code-digest verified at load) may submit a **precompiled program
   plus compiler metadata** through the same service entry;
2. the program is still ISA-checked — the check is what protects the block, and
   a trusted producer does not make a malformed program safe;
3. the module's uniform stream is still driver-built from handles, so it still
   cannot name an arbitrary address;
4. the module is charged GPU memory like any context, and its failures are
   contained like any context's.

That path is not scheduled. It is recorded so that "we might need compute" does
not become an argument for reopening the general boundary later.

---

## 4. Admission: what is checked before anything reaches the GPU

Admission happens in the resident service entry, before a job is queued and
before the driver is called. It is a finite list, and every item has a typed
refusal.

### 4.1 Structural

| Check | Refusal |
|---|---|
| The request's version and size match the interface | `BadRequest` |
| The context handle is live and owned by the caller | `StaleHandle` / `ForeignContext` |
| Every object handle is live, owned by the context, and of the expected kind | `StaleHandle` / `ForeignObject` / `WrongObjectKind` |
| The vertex buffer range is inside the object | `RangeOutOfBounds` |
| The index range is inside the object and every index is below the vertex count | `IndexOutOfRange` |
| The uniform block size equals the program's declared uniform size | `UniformSizeMismatch` |
| The program was compiled for this pipeline's blend and format | `ProgramPipelineMismatch` |
| The target's format matches the pipeline | `TargetFormatMismatch` |
| The target's dimensions are within 4096 in each axis | `TargetTooLarge` |
| A texture's dimensions are within 2048 and its size fits its allocation | `TextureTooLarge` / `TextureTooSmall` |
| The viewport and scissor are inside the target | `ViewportOutOfBounds` |
| The context's memory budget would not be exceeded | `BudgetExceeded` |
| The submission queue is not full | `QueueFull` |
| No read-after-write hazard without an intervening fence | `MissingFence` |

### 4.2 Construction

After admission, the subsystem normalises the request into a job description:
resolved object references, resolved offsets, a state block, a uniform block
descriptor, and the shader metadata. The driver then constructs:

* the bin list: binning configuration, start, primitive format, state records,
  shader state, primitive;
* the shader records;
* the uniform stream, including texture descriptors;
* the render list: render configuration, clear if requested, per-tile
  coordinates, tile-buffer load, branch into the tile's bin list, tile-buffer
  store;
* the semaphore handshake between the two lists.

None of that is visible to the application, and none of it is derived from
application-supplied structure.

### 4.3 Splitting

A draw is split into several jobs when it exceeds a bound:

* the tile count per job (so that a single job's binner memory and render time
  are bounded, and a timeout is meaningful);
* the primitive count per job (the hardware's vertex-count limit, and the
  scheduling granularity);
* the queue's capacity.

Splitting is the driver's and subsystem's decision. The API returns one fence
for the whole draw, which completes when the last split job does. This keeps the
application's model simple while giving the scheduler real interleaving points.

---

## 5. Refusals

### 5.1 Why refusals are the API's normal failure mode

The alternative to a refusal is an approximation: draw something close, or fall
back silently. Both are forbidden here. A 3D API that silently degrades produces
a picture that is wrong in a way the application cannot detect, and this system's
whole conformance story depends on exactness being observable.

Every refusal is typed, names the field or object that caused it, and is
returned before any GPU state changes. The error-invariant defect this project
forbids — "busy" with no holder — applies here as well.

### 5.2 Shader refusal

`create_program` runs the ISA check on the machine code:

| Check | Refusal |
|---|---|
| Every instruction is a legal encoding | `IllegalInstruction` with the instruction index |
| The program length is within the instruction memory | `ProgramTooLong` |
| No instruction violates the guide's restrictions (`docs/QPU-RESEARCH.md` §9) | `RestrictedInstruction` with the index and the rule |
| Every branch target is inside the program | `BranchOutOfRange` |
| The program's declared metadata is internally consistent (attribute count, varying count, uniform size, VPM extents) | `BadMetadata` |
| The VPM allocation does not exceed the VPM the job will have | `VpmOverflow` |
| The uniform stream layout is well-formed and every texture slot has a type the target supports | `BadUniformLayout` |

The check is bounded work: one pass over a program of bounded length. It is the
one place a program crosses the boundary, and it is checked as a program.

### 5.3 What the driver refuses

The driver refuses, by name, a job description it cannot express safely:

* a primitive type the template does not cover;
* a state combination that has no legal configuration-bit encoding;
* a target whose stride or tiling the render configuration cannot express;
* a job whose tile state would not fit the binner pool;
* a VPM layout that collides with the fixed-function pipeline's allocation;
* a request that would require a control-list record outside the template's
  whitelist.

The last one is the important one: the template is a *closed* set of list
shapes. If a job needs a shape that is not in the set, it is refused rather than
hand-built at runtime. That is what keeps "the driver constructs the list" from
degenerating into "the driver has a list builder with arbitrary inputs".

---

## 6. Resources, lifetime and memory

### 6.1 Objects

Every GPU object is:

* owned by exactly one context;
* named by a generation-tagged handle, so a stale handle is refused rather than
  reinterpreted (`system/src/toolbox/handles.rs` is the resident facade over
  `crates/mac-object-table`; the GPU subsystem uses the same discipline);
* accounted in bytes against the owning context and against the device;
* backed by a Device DMA allocation, which supplies both the CPU address and the
  device address.

Physical contiguity: a buffer or texture must be contiguous in device address
space, because the GPU has no scatter-gather. The Device DMA service's
allocation is the unit. Large objects may need to be allocated early or in a
reserved region; this is a board-capacity question (`docs/GPU-SUBSYSTEM.md`
§10), not an API question.

### 6.2 The lifetime rule

**No free precedes completion of work referencing the object.** This is decision
5, and it is enforced structurally:

* a job takes a reference on every object it references, at admission;
* the reference is dropped only when the job's completion is observed;
* `destroy_buffer` and `destroy_texture` drop the *application's* reference and
  return immediately; the object is freed when the last job reference goes;
* `destroy_context` waits for the context's outstanding jobs, then drops all
  references, then frees;
* on crash, the same path runs with a bound; if completion cannot be
  established, the objects are quarantined rather than freed
  (`docs/GPU-SUBSYSTEM.md` §8.3).

### 6.3 Mapping

A buffer may be mapped for CPU access. Mapping is refused while a job that may
write the buffer is outstanding, because the CPU would race the GPU and the
cache maintenance would be wrong in both directions. `unmap` performs the
maintenance:

* before a GPU read of CPU-written data: clean to the point of coherency;
* after a GPU write, before a CPU read: invalidate.

A mapped buffer that the GPU reads while the CPU writes is a bug in the
application, and the API's job is to make it a refusal rather than a corrupted
image.

---

## 7. Synchronisation, readback and failure

### 7.1 Fences

A fence is a monotonic per-context value. `draw` and `clear` return one.
`fence_wait` waits for it with a timeout. Fences are backed by the block's
semaphores where a hardware wait is needed, and by the completion interrupt
otherwise; they are a subsystem resource, not an application-visible semaphore.

The API does not expose "wait for all GPU work" — only per-fence waits — because
a global wait is the operation that turns a slow job into a system-wide stall.

### 7.2 Readback

`readback` requires a fence that covers the write, waits for it, then invalidates
the range and copies. A readback without a covering fence is refused. This is
the only path by which GPU-written bytes become CPU-readable, and it is
deliberately explicit.

### 7.3 Failure modes

| Failure | Detection | What the system does |
|---|---|---|
| **Invalid request** | Admission (§4) | Typed refusal before any GPU state changes. The context is unaffected. |
| **Shader refused** | `create_program` | Typed refusal with the instruction index. No program object exists. |
| **Job refused by the driver** | Construction | Typed refusal naming the shape or field. Nothing was submitted. |
| **GPU hang** | The job's fence does not complete within the timeout | The context is marked failed; the block is reset; the context is killed; its objects are released after the reset; the application gets `DeviceLost`; the rest of the system keeps running. |
| **GPU error bit** (`CTERR`, a DMA error, `ERRSTAT`) | Interrupt handler reads the status | Treated as fatal to the current context, same path as a hang. |
| **Binner out of memory** | `OUTOMEM` | The job is failed with `TileMemoryExhausted`; the context survives; the application may retry with a smaller draw. |
| **Fence timeout on a legal job** | Wait timeout | Same as a hang: the block is presumed wedged. A legal-but-slow shader is indistinguishable from a hung one from the outside, and the safe assumption is a reset. |
| **Reset fails** | Reset does not restore `V3D_IDENT0` | The device is marked dead; the `Rasterise` capability degrades to software; the 3D API returns `DeviceLost` for new contexts; a diagnostic is reported. |
| **Out of memory** | Accounting | `BudgetExceeded` at admission; no partial submission. |

The timeout value is a policy constant, not a per-job parameter. It must be
large enough that a legitimate shader cannot hit it and small enough that a
wedged GPU does not stall the desktop; the first value is provisional and must
be measured (`docs/GPU-SUBSYSTEM.md` §10).

### 7.4 The reset's consequences

A reset loses the block's state, not the system's:

* all queued and in-flight jobs are failed;
* every context that had work outstanding is killed, because its fences will
  never complete;
* objects referenced by those jobs are released only after the reset has been
  observed to succeed;
* the compositor is unaffected, because it does not depend on the V3D — the
  QuickDraw software provider redraws the damaged area and the compositing
  driver presents it;
* a subsequent context can be created and used, unless the reset failed.

The last point is why the design separates `Rasterise` (optional hardware) from
`Compose` (owned by the compositing driver): a GPU failure must degrade drawing,
not the display.

---

## 8. What is not established

1. **The timeout value.** No measurement exists. A shader that legitimately
   takes 50 ms and a shader that is wedged are indistinguishable without a
   measurement of the former.
2. **Whether the 4096/2048 limits are reachable on this board.** They come from
   the vertex format and the TMU; whether the memory and bandwidth exist at
   those sizes is a board question.
3. **The cost of a draw call.** The API is designed on the assumption that a
   draw's fixed cost (list construction, submission, interrupt) is small
   relative to a rectangle fill of a reasonable size. That assumption is
   unmeasured, and it decides whether the QuickDraw path batches 10 operations
   or 1000.
4. **Whether the ISA check can be made complete.** The restriction list in
   `docs/QPU-RESEARCH.md` §9 is the guide's page-37 summary plus the errata. A
   restriction that is real but undocumented would make the check incomplete.
   Resolution: a board run with a deliberately adversarial program per rule.
5. **Physical contiguity at scale.** Whether a large render target and a large
   texture can be simultaneously resident is a board-memory question.
6. **Whether `imul24` is sufficient for the integer multiplies the API needs.**
   The QPU has no 32-bit integer multiply; the compiler synthesises one or
   avoids it. Which shaders need one is not yet known.
7. **Nothing here is testable in QEMU.** The V3D is a placeholder.

---

## 9. Board runs this design needs

1. Bring-up and identification (`V3D_IDENT0`/`IDENT1`), shared with the other
   documents' bring-up.
2. One clear and one triangle into a linear XRGB32 target, read back, and
   compared with the expected bytes.
3. One textured quad, to exercise the uniform stream and the texture descriptor.
4. One blended draw, to exercise the shader-side blend variant.
5. One draw that exceeds the split bound, to exercise splitting and a single
   fence over several jobs.
6. A deliberately long shader, to find the timeout's lower bound.
7. A deliberately wedged job, in a private guest, to exercise the timeout, the
   reset, the context kill and the software fallback.
8. A shader that violates each restriction in `docs/QPU-RESEARCH.md` §9, to
   confirm the check matches silicon.

Under QEMU the block answers zero, so the 3D path's **refusals** are exercisable
and its **draws** are not: context creation must fail with a named cause, the
caller must be able to continue without 3D, and nothing may hang.
`docs/QPU-RESEARCH.md` §3.5 states the general split.

---

## 10. Decisions a reviewer must make

1. **D1 — accept the construction model**, and therefore accept that no
   application may submit a control list or a raw QPU program. §3.2 states the
   cost; §3.2 also states the smallest trusted path if compute is later needed.
2. **D2 — accept the API surface in §1.3**, including what is deliberately
   absent (no mips, no multiple render targets, no instancing, no compute).
3. **D3 — accept that blending is a shader variant** and therefore that
   pipeline state is part of the program key, with a refusal for a mismatch.
4. **D4 — accept that the driver's template is a closed set of list shapes**,
   and that a job needing another shape is refused rather than hand-built.
5. **D5 — accept that a reset kills every context with outstanding work**, not
   just the offending one, because their fences can never complete.
6. **D6 — set the timeout policy** (a constant, and whether it is per job or per
   context).
7. **D7 — decide whether the privileged compute tier is in scope at all**, so
   that the interface's shape is not accidentally closed to it.
