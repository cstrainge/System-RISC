# The kernel GPU subsystem and the V3D driver

Status: design, no code. This document specifies the kernel-side machinery that
owns GPU memory, contexts, submission and failure, and the external driver that
programs the V3D. It is the foundation the two front ends sit on
(`docs/QUICKDRAW-HW-DESIGN.md`, `docs/QUICKDRAW-3D-DESIGN.md`), and it contains
the decision that shapes both of them: **the kernel does not validate arbitrary
control lists; it constructs them** (§6).

Hardware facts are in `docs/QPU-RESEARCH.md`. Nothing here re-derives them.

---

## 1. The shape: a resident subsystem over an external driver

The split follows the project's existing rule — the module/driver core owns
lifecycle, admission and routing; the driver owns device-specific operation:

| Part | Lives in | Owns |
|---|---|---|
| **GPU subsystem** | resident `system/` (a service, not a driver) | Objects, contexts, handles, submission queue, fences, scheduling, accounting, failure policy |
| **V3D driver** | `drivers/v3d/` as an external module | The register window, control-list construction, cache maintenance, the interrupt, reset sequencing |

Why not one piece:

* The subsystem must be reachable by every application, must account memory and
  must survive a driver failure. That is resident policy, not device code.
* The driver must be replaceable and must be the only code that touches the
  block's registers. Keeping it external means a later GPU (V3D 4.x/7) is a new
  driver with the same class, and the subsystem does not change.
* The project's rule that a driver is selected by declared class and claimed by
  a runtime probe applies unchanged.

### 1.1 The driver as a real driver

| Element | Content |
|---|---|
| Crate | `drivers/v3d/`, package name `v3d-gpu` |
| `module.json` | Declares class `GPUX` version 1 in `interfaces`; matches the device on the V3D window's bus key (`brcm,v3d`); declares the services it requires: the V3D clock by name, the V3D reset, the Device DMA allocation service, and the interrupt registration for GPU interrupt 10 |
| `src/lib.rs` | Safe driver body: job construction, object bookkeeping, refusal logic |
| `src/native.rs` | The one audited machine adapter: MMIO window, cache instructions, interrupt acknowledgement |
| `main.rs` | `mac_driver::export_driver!` |
| Platform record | Listed in the record's `install.modules`, so the blessed installation ships it; the device window is declared by the platform lane |

Nothing selects the driver by filename. The kernel selects by class, a probe
claims the device by its declared bus key, and a board with no V3D loads no
driver and reports the capability absent — exactly the rule the timed-output and
video classes already follow.

### 1.2 The class interface

`GPUX` v1, one request/reply record per operation, with the same synchronous
admission discipline as `VDEV`:

| Operation | Input | Reply |
|---|---|---|
| `INFO` | — | Slice/QPU/TMU/VPM/semaphore counts from `V3D_IDENT1`, the `V3D_IDENT0` value, format support, limits |
| `ALLOC` | Bytes, usage flags | Allocation handle, CPU address, **device address**, length |
| `FREE` | Allocation handle | — |
| `CLEAN` / `INVALIDATE` | Allocation handle, offset, length | — (the audited cache adapter) |
| `SUBMIT` | A normalised job description (§5.2) | Job identity |
| `CANCEL` | Job identity | Whether it was still queued |
| `WAIT` | Job identity, timeout | Completed / timed out |
| `RESET` | — | Whether the block came back (`V3D_IDENT0` matched) |
| Completion notification | Job identity, terminal state | Consumed by the subsystem |

The driver does **not** accept a control list, a shader record, a uniform stream
or an address from its caller. Its `SUBMIT` input is the job description that
§5.2 defines, and it constructs everything else itself.

### 1.3 The clock and reset capability this needs

CPRMAN and the PM block are shared infrastructure: they serve the SD card, SPI0,
the RNG and the V3D alike. A driver that pokes their registers directly would
duplicate knowledge across every driver and let one driver change a divider
another depends on. So the design requires a **clock/reset capability**, which
this document specifies and does not implement.

| Operation | Semantics |
|---|---|
| `enumerate` | Which clocks and resets exist, with their names and rates |
| `request_clock(name)` | Returns a refcounted handle; the clock is enabled while any handle is held |
| `release_clock(handle)` | Drops one reference; disabled at zero |
| `clock_rate(handle)` | The current rate |
| `request_rate(handle, rate, tolerance)` | Sets a rate, or refuses naming the holders that depend on the current one |
| `request_reset(name)` | Returns an exclusive handle to a reset line |
| `assert_reset` / `deassert_reset` | Drives the line |
| `release_reset(handle)` | Drops the handle; a line with a holder cannot be asserted by another |

Acceptance criteria:

1. Two drivers requesting the same clock both succeed; the clock stays enabled
   until both release.
2. A rate change that would affect another holder is refused, and the refusal
   names the holder and the resource.
3. A reset line is exclusive: a second `request_reset` for the same line is
   refused while the first is held.
4. No driver programs a CPRMAN or PM register directly; the capability is the
   only writer.
5. The V3D driver's bring-up works with only these operations, with no
   hardcoded CPRMAN offset.

The last criterion is what makes the capability real rather than decorative: the
V3D driver is its first client, and it must not contain a single CPRMAN
address.

---

## 2. GPU memory objects

### 2.1 The object

| Field | Content |
|---|---|
| Handle | Generation-tagged, owned by a context, allocated through the shared object-table discipline (`system/src/toolbox/handles.rs` over `crates/mac-object-table`) |
| Kind | Buffer, texture, render target, program, command buffer, binner slot |
| Size | Bytes |
| CPU address | From the Device DMA allocation |
| Device address | From the Device DMA allocation — the only address the GPU may see |
| Mapping state | Mapped / unmapped, and the mapped range |
| References | The application's reference, plus one per in-flight job |
| Accounting | Bytes charged to the context and to the device |

### 2.2 Contiguity

The GPU has no scatter-gather: a buffer or texture must be contiguous in device
address space. The Device DMA allocation is therefore the unit of allocation,
and an object is one allocation. There is no paging and no relocation: **an
object's device address never changes for its lifetime.** That is a requirement,
not an implementation detail — the control lists and shader records name
addresses, and a relocation would invalidate every list that references them.

### 2.3 Mapping and cache

* A buffer may be mapped for CPU access; the mapping is a CPU address from the
  same allocation.
* Mapping is refused while a job that may write the range is outstanding.
* `unmap` performs the maintenance: clean before the GPU reads CPU-written data;
  invalidate after the GPU writes, before the CPU reads.
* The maintenance is performed by the driver's audited adapter, not by
  application code and not by the subsystem, because it is the one place where
  an architecture instruction must be issued and reviewed.
* A render target that the compositing driver will read needs the same
  invalidate on the compositor's side; that is interface gap G2 in
  `docs/QUICKDRAW-HW-DESIGN.md` §9.2, not something this subsystem can do for
  another owner's mapping.

### 2.4 The lifetime rule

**No free precedes completion of work referencing the object.** Structurally:

1. At admission, a job takes a reference on every object it names.
2. The reference is released only when the job's completion is observed.
3. An application's destroy drops only the application's reference.
4. The object is freed when the reference count reaches zero, and not before.
5. A context destroy waits for the context's jobs, then drops all its references.
6. If completion cannot be established, the object is quarantined (§8.3), not
   freed.

The reference is taken **before** the job is queued, not when it starts, because
a job can be queued for an unbounded time and the application may destroy the
object in the meantime.

---

## 3. Bring-up contract

The first stage is the cheapest proof that the block is alive, and it precedes
any control list:

1. `request_clock("v3d")` — a refcounted enable. Absence is a hard failure: the
   driver cannot program the block without its clock, unlike Linux, which treats
   it as optional because PM runtime covers it.
2. `request_reset("v3d")`, `deassert_reset` — release the block.
3. Read `V3D_IDENT0`. It must be `0x02443356`. Anything else means the block is
   not the one the record declared, or the clock/reset sequence did not take;
   release everything and refuse to attach.
4. Read `V3D_IDENT1` and derive the slice, QPU, TMU, semaphore and VPM counts.
   These become the capability record that the rest of the system uses.
5. Clear the L2 (`L2CACTL`), set `VPMBASE` to the chosen partition (§10),
   zero `BPOA`/`BPOS`, enable the interrupts the driver handles
   (`INTENA`: `FRDONE`, `FLDONE`, `OUTOMEM`).
6. Register the interrupt handler for GPU interrupt 10.
7. Allocate the command-list storage and the binner pool slot.

Teardown is the reverse: disable interrupts, wait for idle with a bound, reset
the block, release the reset, release the clock. `BPOA`/`BPOS` are zeroed at
teardown so a later driver does not inherit an overflow address that points into
freed memory.

The probe's success criterion is not "the register write stuck". QEMU's V3D is a
placeholder that returns zero, and the 2D design already records the general
lesson: identification must come from the identity register and the platform
record, never from a read-back of a register that a model might implement as
plain memory.

---

## 4. Contexts and namespaces

A context is:

* a handle namespace — every object the context creates is reachable only
  through it;
* a queue of pending jobs;
* a fence counter;
* a memory budget and current usage;
* a priority;
* a failure state (alive, failed, killed).

Rules:

1. **Objects are owned by exactly one context.** A handle from another context
   is refused as foreign, not reinterpreted. This is the same rule the resident
   object table already enforces for other services.
2. **Handles are generation-tagged.** A stale handle — one from a destroyed
   object, or from a previous incarnation of the same slot — is refused.
3. **Fences are per context and monotonic.** A fence value from another context
   is refused.
4. **A context cannot name another context's memory**, because it cannot name a
   device address at all; it names handles, and the subsystem resolves them
   within the context's namespace.
5. **A context's failure does not affect another context**, except that a reset
   of the block fails every context with outstanding work (§8.2), because their
   fences can never complete.

The namespace is the primary defence. An application that guesses a handle
number gets a different object or a refusal, never another application's buffer,
because the resolution is scoped to the caller's context.

---

## 5. Submission

### 5.1 One queue, one job at a time

The block has two list processors (binning and rendering) and runs one pair of
lists at a time. There is no hardware job queue and no job tag: a completion
interrupt tells you that *the* job finished, not *which*. So:

* the subsystem keeps a software queue per context and a single device queue;
* the driver knows which job it has submitted, because it submitted it;
* completion is matched by identity in the driver, not by the hardware.

This is a simplification, not a limitation to be worked around: it means the
submission path is serialised at the block, and the interesting scheduling
question is *which job next*, not *how to overlap two jobs*.

### 5.2 The job description

The subsystem normalises an admitted request into a fixed-layout description and
hands it to the driver:

| Field | Content |
|---|---|
| Target | Object handle, width, height, stride, format, tiling, device address |
| Clear | Colour and depth values, or "load the existing contents" |
| Shaders | Program handles, with the compiler's metadata (attribute, varying, uniform and texture-slot layouts) |
| Vertex data | Buffer handle, offset, stride, count, attribute layout |
| Index data | Buffer handle, offset, count, index width, or "none" |
| Uniforms | A block of scalar values, in the program's declared order |
| Textures | One entry per declared slot: handle, format, dimensions, device address |
| State | Depth test/write, cull, viewport, scissor, blend variant |
| Split | Which slice of the draw this job covers (first vertex, count) |

The driver turns this into control lists from a **closed template** (§6.3). The
description contains no list, no record, no address the subsystem did not
resolve, and no shader code the ISA check did not pass.

### 5.3 Completion

The render list's completion sets `INTCTL.FRDONE`. The driver's handler:

1. reads `INTCTL` and records which bits were set;
2. clears them;
3. maps `FRDONE` to "the current job completed", `FLDONE` to "its binning pass
   completed" (informational), `OUTOMEM` to a job failure, and any error bit to a
   context failure;
4. posts the completion to the subsystem with the job identity;
5. submits the next job, if any.

The handler does no work beyond that: no allocation, no blocking, no application
callbacks. That is the project's bounded-interrupt rule.

### 5.4 What is borrowed from the timed-output class, and what is not

The timed-output class (`docs/TIMED-OUTPUT-CLASS.md`) is the project's job
model: claim an exclusive resource, submit a buffer with parameters, receive a
job identity, learn completion from the hardware rather than from an elapsed
time, cancel, release. GPU submission adopts that model. It does **not** adopt
the class's DMA feeder, and the reason is hardware, not preference:

* A serialiser cannot fetch its own data, so a DMA channel must stand between
  the buffer and the peripheral. **The V3D can fetch its own data**: it is a bus
  master that reads control lists, shader code, vertex data and textures
  directly. Putting a DMA channel in the submission path would add a mechanism
  that moves bytes the GPU already moves itself, and would introduce a second
  owner of the same transfer.
* What the class contributes is the **shape** of the contract: exclusive claim,
  a job identity, hardware-sourced completion, cancellation that returns the
  resource to a defined state, and refusal that names the holder. The GPU
  subsystem's contexts are clients of one exclusive block, exactly as
  serialiser clients are clients of one exclusive serialiser.
* The class's **lease ledger** is the model for a multi-unit resource, not a
  requirement here: the block is one device with one owner. Where a DMA channel
  *is* used — pre-clearing or pre-filling a buffer — it must be leased from the
  same pool through the resident arbiter (`system/src/drivers/leases.rs`, pool
  mask `0x7f35`), never claimed as a page. That keeps the one arbitration point
  for DMA channels that the class already establishes.

---

## 6. The decision: construct, do not validate

### 6.1 The threat, stated exactly

There is no IOMMU. The GPU is a bus master that reads and writes physical
addresses. A control list is a program: it names addresses for the binner, the
tile buffer, vertex and index data, shader code, shader records and uniforms,
and the uniform stream contains texture descriptors that name further addresses.
A wrong physical address is simply a wrong address: the GPU will read or write
whatever is there, and the ARM sees no fault.

Linux's validator says this in its own file comment, and it is the clearest
statement of the problem: without an IOMMU, a user with control-list access could
escalate privilege by drawing to system memory as if it were a framebuffer, or
read memory it should not by naming it as a vertex or index buffer.

### 6.2 Why validating arbitrary lists is close to proving a program safe

The previous draft of this design said "submission is validated at the kernel
boundary". That was the naive form, and it is withdrawn. To accept an arbitrary
list, a validator must:

* whitelist every record type and check each record's length against the list;
* track shader records, their sizes, their alignment and their attribute counts;
* bound index-buffer access and track the maximum index per shader state;
* validate binner configuration, tile-state size and tile-alloc size;
* relocate every address in the list, in the shader records and in the uniform
  stream;
* decode every texture descriptor in the uniform stream and check its size,
  tiling and mip chain against the buffer it names;
* check uniform-buffer offsets and clamps so a shader cannot read outside them;
* reject unsupported flags and combinations;
* and validate the shader instruction stream separately, because a shader is a
  second program the hardware executes.

That list is not hypothetical: it is what mainline does, across
`vc4_validate.c` and `vc4_validate_shaders.c`, and each item is a place where a
missed check is a privilege escalation. A validator that accepts *general* lists
is therefore either unsound (it misses a case) or unusable (it rejects so much
that applications cannot express anything). It is close to proving an arbitrary
program safe, and that is not a boundary this system should own.

**Withdrawn:** "validate arbitrary control lists at the kernel boundary".

### 6.3 The alternative: the kernel constructs the lists

The application submits **parameters** — a draw call: vertex buffer and count,
shader handle, render target, state values, uniforms. The subsystem checks the
parameters; the driver emits the control lists itself from a constrained
template.

Why this is tractable rather than a leap of faith:

1. **The shader is confined to an ISA subset with no addressing capability.**
   The hardware *does* give a shader routes to memory — the VPM's
   general-purpose DMA mode, and direct-addressed TMU lookups — and Linux's
   shader validator says so in its own threat statement. The design therefore
   does not claim "a shader cannot name an address"; it requires that the
   compiler emit a subset that has no such route, and that the kernel check the
   subset was used:
   * the VPM configuration and address registers are written only with
     compiler-emitted immediates selecting register-file transfer, never
     general-purpose DMA;
   * texture sample metadata never declares direct addressing, and the driver
     refuses a program whose metadata does;
   * the uniform stream is driver-built, so a uniform read names only what the
     driver chose.
2. **Every address-bearing structure is driver-built.** Control lists, shader
   records, uniform streams, texture descriptors, binner tile state and tile
   alloc are all constructed from handles and bounds the subsystem resolved. The
   application contributes no structure, and never learns a device address.
3. **The only program crossing the boundary is the shader**, and a shader is a
   bounded, fixed-encoding instruction stream with a small ISA. Checking it is a
   pass over a bounded number of 64-bit words: instruction legality, the
   restriction list (`docs/QPU-RESEARCH.md` §9), branch range, metadata
   consistency, and the subset rules above. Mainline does the same job in a
   separate shader validator, and it is the precedent for this being a bounded
   check rather than a general proof.
4. **The template is a closed set of list shapes.** The driver does not have a
   general list builder with application-controlled inputs; it has a small set of
   shapes, one per expressible draw, and it refuses anything outside the set.

So the boundary validates **data** — ownership, liveness, kinds, ranges, budgets
— and admits exactly **one** kind of program: a shader in a subset with no
addressing capability. The thing that made the naive design impossible — an
arbitrary program with arbitrary addresses — is never accepted.

### 6.4 Residual risk, stated rather than papered over

Construction moves the problem; it does not delete it. The remaining risks are:

| Risk | Consequence | Mitigation |
|---|---|---|
| A bug in the template | The list names a wrong address or a wrong length | The template is small, one shape per draw, and reviewed as the security-critical code it is. Structural assertions run before submit: every address is inside an object the job references; every length is inside its object; the tile-state and tile-alloc regions are inside the driver's pool. |
| An incomplete subset check | A shader could reach memory through the VPM's DMA mode or a direct TMU lookup | The subset is deliberately small and structural: immediates for the VPM configuration, metadata for the TMU. The checker rejects computed writes to the configuration registers and any direct-address sample declaration. An adversarial program per known route is a required board test. **This is the residual memory-safety risk, and it is not eliminated.** |
| An incomplete instruction-restriction check | A program that violates a hardware rule | The check is conservative: it accepts a **subset** of the ISA, and the compiler only emits that subset. An instruction the checker does not know is refused. The consequence of a missed restriction is a wrong picture or a hang, not arbitrary memory access. |
| A metadata bug in the compiler | Wrong VPM layout, wrong uniform order, wrong varying count | The metadata is validated against a fixed schema at `create_program`, and against the job's VPM budget at admission. The consequence is a wrong picture or a refused job. |
| A bug in address resolution | The driver resolves a handle to the wrong object or a wrong offset | Resolution happens in one function, with the bounds check adjacent to the resolution, and the job holds a reference to the resolved object. This is the one place a memory-safety bug is possible, so it is the one place that gets the most review. |
| A binner configuration bug | The binner writes per-tile lists outside its pool | The tile-state and tile-alloc values come from the subsystem's own pool allocation, never from the application, and the driver asserts the region is inside the pool before programming it. |
| A legal but pathological shader | A hang | Timeout, reset, context kill (§8). This is an availability failure, not a memory-safety one, and it is contained. |

The honest summary: construction reduces the attack surface from "prove an
arbitrary program safe" to "prove one small template correct, one subset check
complete, and one address-resolution function correct". That is reviewable. It
is not zero, and the design does not claim it is.

### 6.5 Availability is a separate property

A legal shader can still hang the GPU — a loop whose uniform condition never
becomes false. Nothing in the construction model prevents that, and nothing
should pretend to: the answer is a timeout, a reset and a killed context, which
§8 specifies. Memory safety and availability are different claims and are
reported separately.

### 6.6 Why not a "slightly general" list

A middle ground — an application-authored list restricted to a safe record set —
was considered and rejected. It still needs the relocation and descriptor
parsing of the full validator (because the safe records still carry addresses),
and the restricted set converges on the template anyway, while adding a parser
and a whitelist to review. It is the worst of both: the attack surface of a list
with the rigidity of a template.

### 6.7 The trusted path, if compute is ever needed

Not scheduled. The smallest extension is a privileged module (signed, loaded
from the blessed directory, digest-verified) submitting a precompiled program
plus metadata through the same entry, with the program still ISA-checked and the
uniform stream still driver-built. That is stated in
`docs/QUICKDRAW-3D-DESIGN.md` §3.2 and is repeated here because it is the only
acceptable way to widen the boundary: widen the *producer's* trust, not the
*accepted structure*.

---

## 7. Fences and synchronisation

* A fence is a monotonic per-context sequence number, completed by the driver
  when the job's `FRDONE` arrives.
* Fences are software; the block has no job tag, so there is nothing to wait on
  in hardware for a job.
* The hardware semaphores are used **inside** a job, for the binning-to-render
  handshake, and their count is bounded by `V3D_IDENT1`. They are not exposed to
  applications.
* `WAIT` has a timeout. A wait that times out is treated as a hang (§8.2),
  because a legitimate job and a wedged one are indistinguishable from outside.
* A readback requires a fence that covers the write, then an invalidate, then
  the copy.

If the design later overlaps jobs, hardware semaphores become the fence
mechanism; that is a deliberate non-goal for the first landing, because a single
job at a time makes the failure analysis tractable.

---

## 8. Failure

### 8.1 Typed refusals

Every admission failure is typed and returned before any GPU state changes. The
list is in `docs/QUICKDRAW-3D-DESIGN.md` §4.1. The rule that matters here is that
a refusal never leaves a partially submitted job: the job is queued only after
every check passes, and object references are taken before queueing.

### 8.2 Hang, timeout, reset

1. The job's fence does not complete within the timeout.
2. The subsystem stops dequeuing for the device and marks the **current
   context** failed.
3. The driver resets the block: stop both list processors, wait for idle with a
   bound, disable interrupts, assert the reset line, release it, re-enable the
   clock, re-read `V3D_IDENT0`, re-initialise (`VPMBASE`, `INTENA`,
   `BPOA`/`BPOS`).
4. If `V3D_IDENT0` matches, the reset succeeded: every context with outstanding
   work is killed (their fences can never complete), their objects are released
   now that the reset has been observed, the queue is drained of killed
   contexts' jobs, and the device returns to service.
5. If it does not match, the device is marked **dead**: `Rasterise` degrades to
   software, new contexts are refused with `DeviceLost`, and a diagnostic is
   reported. The compositing driver is unaffected.

Killing every context with outstanding work, not just the offending one, is
deliberate: after a reset there is no way to know which jobs ran, and a fence
that will never complete is worse than a killed context.

### 8.3 Quarantine

If a reset fails, or if a job's completion cannot be established, the objects it
referenced are **quarantined**:

* they are removed from the context's namespace and marked unreclaimable;
* their bytes stay charged to the device, not to the dead context;
* a diagnostic records the object, its size and the job that referenced it;
* a later successful reset may reclaim them.

Freeing memory that a possibly-in-flight job references is the one thing that
must never happen, so quarantine is the fallback on every path where completion
is uncertain — including process crash, context kill and reset failure.

### 8.4 Crash

A process that dies with work outstanding follows the same path as a kill: the
context is marked failed, its jobs are cancelled if still queued, its in-flight
job is waited for with a bound, and the objects are released after completion or
quarantined. The QuickDraw interposer's crash path
(`docs/QUICKDRAW-HW-DESIGN.md` §6.4) is a client of this.

---

## 9. Scheduling and accounting

### 9.1 Scheduling

* The device runs one job at a time.
* Contexts have a priority: the foreground application above background work.
* Within a priority, round-robin, with a bound on consecutive jobs so one
  context cannot starve another.
* There is no preemption: a job runs to completion or to a timeout. Job size is
  therefore a scheduling parameter, which is why draws are split
  (`docs/QUICKDRAW-3D-DESIGN.md` §4.3).
* A killed or failed context's jobs are removed from the queue without running.

The scheduler's unit is a job, not a draw. A draw that splits into five jobs
gives the scheduler four extra interleaving points, and that is the reason to
split beyond the hardware limits.

### 9.2 Accounting

| Charge | Unit |
|---|---|
| Object memory | Bytes, per object, per context |
| Command-list storage | Bytes, per job, per context |
| Binner slot | Bytes of the pool, per job |
| Quarantined objects | Bytes, per device |

* A context has a budget; exceeding it is a refusal at admission, before any
  allocation.
* The device has a total budget; exceeding it is a refusal, and the refusal
  names which contexts hold the memory.
* Freeing returns the charge. Quarantine does not return the charge until the
  object is reclaimed.
* The binner pool is allocated once, at bring-up, and carved into slots; a job
  that cannot get a slot waits or is refused, and the refusal names the pool.

---

## 10. Open design questions this subsystem owns

1. **`VPMBASE` partition.** Linux writes zero: the whole VPM to the
   fixed-function pipeline, none to standalone QPU programs. This design uses
   the fixed-function pipeline only, so zero is the natural first value — and it
   forecloses raw compute. A non-zero partition would allow compute but reduce
   the pipeline's VPM, with an unquantified effect on varying capacity. Decision
   D1 below.
2. **The timeout.** No measurement exists. It must be large enough for a
   legitimate shader and small enough that a wedged GPU does not stall the
   desktop. Provisional: 250 ms, to be measured.
3. **Command-list storage.** Per job from a pool, or one buffer per context with
   double buffering. The choice affects how long a list must stay alive after
   completion, which affects the free-after-completion rule.
4. **Binner pool size.** Linux uses 16 MiB, 512 KiB slots, and checks the
   256 MiB boundary rule. This board's memory budget may not allow 16 MiB
   resident; a smaller pool with more slots, or a pool allocated on demand, is a
   board question.
5. **Whether a context can hold more than one surface.** The first landing uses
   the compositing driver's surface as the target, which means a GPU context and
   the compositor share an object. That is a cross-owner lifetime problem, and
   it is gap G3 in `docs/QUICKDRAW-HW-DESIGN.md` §9.2.

---

## 11. What is not established

1. **Whether the clock/reset capability can be implemented without a new
   platform record entry.** The record declares CPRMAN and PM; whether a
   named-clock mapping exists is a record question for the platform lane.
2. **The block's reset behaviour.** Whether the PM reset line fully clears the
   V3D, or whether some state (L2, VPM, semaphores) survives and must be cleared
   explicitly, is a board question. The design clears L2 and re-initialises
   everything it knows about, but "everything it knows about" is not a proof.
3. **Whether a reset can be observed as complete.** `V3D_IDENT0` matching is
   necessary, not sufficient; a board test that runs a known job after a reset
   is what establishes it.
4. **The timeout.** See §10.2.
5. **Memory capacity.** Whether a render target, a texture and a binner pool can
   be resident at once, at the sizes the design wants.
6. **The cost of the interrupt path.** One interrupt per job, with a job per
   draw (or per split), may be too many interrupts for a desktop's frame rate.
   Unmeasured.
7. **Nothing here is testable in QEMU.** The V3D is a placeholder returning zero
   at an address that is not the silicon's.

---

## 12. Board runs this subsystem needs

1. Bring-up: clock, reset, `V3D_IDENT0` = `0x02443356`, `V3D_IDENT1` counts.
2. One allocation, one job, one completion interrupt, one fence.
3. Two contexts, interleaved, to prove the namespace and the scheduling.
4. A context destroyed with a job in flight, to prove the free-after-completion
   rule (the object must not be freed early, and the job must still complete).
5. A process killed with a job in flight, to prove the crash path.
6. A deliberately wedged job, to prove the timeout, the reset and the context
   kill — in a private guest.
7. A reset that fails (by holding the reset line), to prove the degraded path
   and the quarantine.
8. A memory-budget exhaustion, to prove the refusal and its naming.

---

## 13. Decisions a reviewer must make

1. **D1 — accept construction over validation** (§6), including the loss of
   application-authored lists and raw compute, and the residual risk in §6.4.
2. **D2 — accept `VPMBASE = 0`** for the first landing, and therefore that raw
   compute is not available.
3. **D3 — accept a single job at a time**, with software fences and hardware
   semaphores used only inside a job.
4. **D4 — accept that a reset kills every context with outstanding work.**
5. **D5 — accept quarantine as the fallback** when completion is uncertain, and
   that quarantined memory stays charged.
6. **D6 — accept the driver/subystem split** as specified in §1, including the
   `GPUX` class and the clock/reset capability.
7. **D7 — set the timeout** and the scheduling quantum.
