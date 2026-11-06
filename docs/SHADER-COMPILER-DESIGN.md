# A Rust shader compiler: front end, target-neutral IR, and one backend per GPU

Status: design, no code. This document specifies the compiler crate that turns a
shader into machine code for a target GPU, with VideoCore IV QPU as the first
backend. It is consumed by `docs/QUICKDRAW-3D-DESIGN.md` (§2.2 is the ABI the
two share) and by `docs/GPU-SUBSYSTEM.md` (§6.3 is the ISA check the compiler's
output must survive).

The split the brief asks for is the right one and is taken as given: **one front
end, one target-neutral IR, one backend per hardware target.** The interesting
decisions are what the front end accepts, what the IR guarantees, and what the
QPU backend must do that no ordinary compiler backend has to do.

---

## 1. What the compiler is for

Three consumers, and they constrain the design more than any language preference
does:

1. **QuickDraw shaders.** Rectangle fills, blits, pattern fills, glyph runs. All
   integer, all pixel-exact, all small. These are written once, by this project,
   and must compile to something that matches software byte for byte.
2. **The 3D API's shaders.** A vertex shader and a fragment shader for a small
   set of scenes: transformed, textured, lit, blended.
3. **Future targets.** The same source must compile for a later SoC's GPU without
   the source changing. That is the whole reason for the IR.

What it is *not* for: running arbitrary GLSL from the wild, or matching a
desktop GPU's floating-point behaviour. Both would be scope the system does not
have and cannot verify.

---

## 2. What the front end accepts

### 2.1 The options

| Option | Attraction | Cost here |
|---|---|---|
| **A GLSL-like subset** | Familiar syntax; existing mental model; examples transfer | Imports GLSL's semantics wholesale: implicit conversions, precision qualifiers, derivatives, `discard`, undefined behaviour around NaN, a large grammar. Every one of those is a decision the QPU cannot honour or that this project would have to redefine. The result is "GLSL, except…", which is worse than a language that says what it means. |
| **SPIR-V as the only input** | Standard binary; reuse glslang / rust-gpu / naga; no parser to write | SPIR-V assumes storage classes, pointers, images, atomics, memory barriers and a runtime model the QPU does not have. A SPIR-V front end would spend most of its code rejecting things, and it would still need a validator to be safe against malformed modules. It is the right *second* input, not the first. |
| **A small purpose-built language** | Exactly the semantics the target can honour; small front end; every construct has a defined lowering; errors are meaningful | No ecosystem; every shader is written by this project (which is already true for the first two consumers). |

### 2.2 The decision

**A small, statically typed language with GLSL-like expression syntax, compiled
by a hand-written front end; the IR is designed so a SPIR-V importer can be
added later without touching any backend.**

The reasoning:

* The first two consumers are *this project's own shaders*. The value of
  accepting GLSL is that other people's shaders work, and there is no such
  corpus here.
* Pixel-exactness requires integer semantics that are *defined*, not inherited.
  A purpose-built language can say "i32 addition wraps; there is no undefined
  behaviour; float is IEEE-754 binary32 except that denormals may flush to zero
  and the compiler will tell you when that can matter". GLSL says much less.
* The QPU cannot express divergence, recursion, function calls with a stack,
  or general memory. A language that cannot express them produces a compiler
  whose only failure mode is a *refusal*, not a miscompile. That is the same
  discipline as the rest of the system.
* SPIR-V remains reachable: a SPIR-V importer lowers into the same IR, and the
  IR's capability checks reject what the target cannot do. Nothing in the
  backends changes.

### 2.3 The language, in outline

Deliberately small:

* **Types:** `bool`, `i32`, `u32`, `f32`, and vectors `vec2/3/4` of each; plus a
  packed `u8x4` type for four 8-bit channels in one 32-bit register. No
  pointers, no references, no arrays except a small fixed-size local array that
  must be fully unrolled, no structures beyond the shader interface blocks.
* **Functions:** defined, always inlined, no recursion. A call graph with a
  cycle is a compile error, not a runtime problem.
* **Control flow:** `if`/`else` and `loop`. An `if` condition must be either
  **uniform** (a value that is the same for all elements — a uniform, a
  constant, or a comparison of those) or it is lowered to a **select**. A
  data-dependent `if` that cannot be lowered to a select is an error, because
  the QPU cannot diverge. A `loop` must have a uniform trip count or a
  uniform exit condition, for the same reason.
* **No implicit conversions.** Every conversion is written (`f32(x)`,
  `u32(x)`), so the rounding and saturation are visible at the source.
* **Explicit shader interface:** vertex attributes, varyings, uniforms,
  textures and outputs are declared with their types and their *slot*, because
  the driver needs a stable layout (§2.2 of the 3D design).
* **Integer operations are exact and total.** Wrapping add/sub/mul, defined
  shift amounts, no traps.
* **`discard` and derivatives are absent.** The first target cannot implement
  derivatives (no quad-level neighbour access), and `discard` on a tiled
  renderer changes the tile-buffer protocol. Both are later decisions.

### 2.4 What the front end rejects, by name

Recursion; a call graph cycle; a non-uniform `if` that cannot be selected; a
non-uniform loop bound; a local array that cannot be fully unrolled; a vector
width the target does not support; a texture format the target does not sample;
a shader whose uniform block exceeds the target's uniform budget; a shader whose
register pressure exceeds the target's file (see §4.6); a shader that needs a
64-bit integer; a shader that needs a 32-bit integer multiply on a target whose
only multiply is 24-bit and whose correction path was not selected.

Each rejection names the construct and the target capability that failed, in the
same spirit as the device classes' refusals.

---

## 3. The IR

### 3.1 Shape

SSA, typed, in a structured-control-flow form:

* **Values** are typed, and every value is defined once.
* **Instructions** are pure except for a small, explicit set of effects:
  texture fetch, tile-buffer read, tile-buffer write, varying read, and
  output write. There is no general memory.
* **Control flow** is structured: a function body is a tree of blocks with
  `if`/`loop`/`break`/`continue`/`return`. Not an arbitrary CFG. This is a
  deliberate restriction: every backend in scope has branch restrictions
  (delay slots, no divergence, no call stack), and a structured form makes the
  legal lowerings enumerable.
* **Vector types are first-class**, but the IR does not fix a hardware width.
  A `vec4<f32>` is a value; the backend decides whether it is one register per
  element or a packed register.
* **The packed `u8x4` type is first-class** because it is the difference
  between four pixels per instruction and one, and it is expressible on any
  target (a target that cannot pack lowers it to four scalar operations).

### 3.2 Why it is target-neutral

The IR contains no:

* register numbers, register counts or register classes;
* VPM addresses, uniform offsets, attribute slots or varying slots as *numbers*
  (it contains them as *named* interface elements; the backend assigns the
  numbers and reports them as metadata);
* instruction encodings, opcodes, delay slots, scoreboards;
* knowledge of tiling, tile-buffer layout or the render pipeline;
* target-specific integer widths beyond the declared type.

What it *does* contain is what the target must be able to do: types, arithmetic,
control flow, and the five effects. A backend's job is to lower those onto its
hardware, and a backend that cannot is expected to say so through its capability
declaration (§6) rather than to approximate.

### 3.3 Verification

The IR has a verifier that runs before any backend:

* every use is dominated by its definition;
* types agree at every use;
* every effect is in a legal position (a tile-buffer write only in a fragment
  shader, a varying read only in a fragment shader, and so on);
* every interface element declared by the shader is either read or written
  consistently with its direction;
* no value escapes a block it is not defined in;
* structured control flow is well formed (no irreducible loops, no `break`
  outside a loop).

A backend may then assume these properties and does not re-check them.

---

## 4. The QPU backend

The backend has six jobs that an ordinary compiler backend does not: no
divergence, VPM allocation, a uniform stream, TMU scheduling, delay slots, and
register allocation with no spill. Each is stated with the hardware fact that
forces it (`docs/QPU-RESEARCH.md` §7 and §9).

### 4.1 Lowering to a 16-wide SIMD with no divergence

The QPU executes sixteen elements per instruction and branches as a whole. So:

* **`if` with a uniform condition** becomes a branch, with the two delay slots
  filled from the fall-through or the target block, subject to §4.5.
* **`if` with a non-uniform condition** becomes predicated execution: every
  instruction in both arms is emitted with a condition code, and the two arms
  are merged by select. The IR's structured form makes this a local
  transformation.
* **`loop`** must have a uniform exit; the backend emits a conditional branch
  with the comparison on a uniform value.
* **`select`** is a pair of predicated moves, or an ALU operation with a
  condition code, whichever the scheduler prefers.

The front end's rule that non-uniform control flow is an error is what makes
this total: the backend never has to invent a divergence strategy.

### 4.2 VPM allocation

Varyings from the vertex shader and reads by the fragment shader live in the
VPM. The backend:

* assigns each varying a VPM offset in the vertex shader's output region and a
  matching offset in the fragment shader's input region;
* assigns each vertex attribute an offset in the VPM region the fixed-function
  pipeline fills from the attribute records;
* reports the attribute offsets, the varying offsets and the total sizes as
  metadata, because the driver writes them into the shader record;
* checks the total against the VPM the job will have, and refuses if it does not
  fit.

VPM is also the only "large" storage a shader has, and it is shared with the
fixed-function pipeline. `VPMBASE` partitions it globally
(`docs/QPU-RESEARCH.md` §3.3), so the backend's VPM budget is a parameter of the
compile, supplied by the driver from the current partition — not a constant.

### 4.3 The uniform stream

Uniforms are read sequentially through a pointer the shader sets, and the stream
is shared with the texture descriptors. The backend:

* decides the order in which uniforms are read, and therefore the layout of the
  stream;
* emits the `uniforms address` write that sets the pointer, and the reads in
  that order;
* reports the layout as metadata, so the driver writes the stream in the same
  order;
* allocates a slot for each texture descriptor and reports its offset and type.

Two hardware behaviours matter: the uniform pointer is per-QPU state that must be
reset for each program invocation (the read position advances), and the uniform
read is a broadcast value shared by all sixteen elements. The backend must not
schedule a uniform read in the last three instructions of a program
(`docs/QPU-RESEARCH.md` §9.2).

### 4.4 TMU scheduling

A texture fetch writes coordinates to the TMU, then a later instruction with the
`load TMU0`/`TMU1` signal moves the result into `r4` and then the destination.
The result's latency is not fixed, so the backend:

* issues the fetch, then schedules independent work, then the load;
* respects the `TMU_NOSWAP` spacing rule (three instructions before the first
  dependent write);
* respects the `r4` restrictions after a load and after an SFU operation;
* uses both TMUs when two fetches are independent.

### 4.5 Branches and delay slots

* Every branch has two delay slots that execute unconditionally. The backend
  fills them with instructions that are safe on both paths, or with nops.
* The last three instructions of a program (thread end plus two delay slots)
  are subject to the restrictions in `docs/QPU-RESEARCH.md` §9.2: no varying,
  uniform or VPM access, no register 14, no Z write in the final slot.
* The first two instructions of a fragment shader must not wait on the
  scoreboard, which rules out a tile-buffer access there.

### 4.6 Register allocation with no spill

The register file is five accumulators plus 64 A/B registers, and only the
bottom 16 A/B registers are available when two fragment-shader threads share the
file. There is no memory to spill to: a spill would have to go through the VPM,
which is a protocol surface, not a scratchpad, and through a load/store the
instruction set does not have for that purpose.

So the backend's register allocator has **no spill path**. When pressure
exceeds the file, it may:

1. **recompute** a value rather than keeping it live (rematerialisation of
   constants and cheap expressions);
2. **coalesce** by reordering so that live ranges shorten;
3. **split the shader** into stages that communicate through the tile buffer,
   where the algorithm allows it (a blend can be done in two passes);
4. **refuse**, naming the value whose live range could not be allocated and the
   pressure at that point.

Refusal is a first-class outcome, not a crash. A shader that needs more than the
file is a shader the target cannot run, and the front end must be able to say so
before the user sees a wrong picture.

The allocator also observes two QPU-specific classes: `r4` cannot be written as
a general-purpose register (it is the pack/unpack register), and register 14 is
reserved by the hardware's addressing quirk that implementations work around.
Both are constraints on the allocation, not on the IR.

### 4.7 Integer arithmetic and the missing multiply

There is no 32-bit integer multiply. The backend:

* lowers `i32`/`u32` multiplication by `imul24` plus correction when the
  operands are known to fit 24 bits (the common case: coordinates, phases,
  small counts);
* otherwise synthesises a full 32-bit product from partial products, at a cost
  the backend reports;
* and refuses if the target capability record says the synthesis is not enabled
  for this compile.

The QuickDraw shaders are designed to avoid multiplication entirely: a pattern
phase is a shift and a mask, a glyph offset is an add, a pixel pack is a shift
and an or.

### 4.8 Packing for 8-bit work

Four 8-bit pixels in one register is the difference between one instruction per
pixel and one per four. The backend maps the IR's `u8x4` type onto pack/unpack
modes, and must handle the guide's mislabelled Table 8 by using the encodings
implementations actually emit (`docs/QPU-RESEARCH.md` §7.8).

### 4.9 Output

The backend emits:

* the machine code, as 64-bit words;
* the metadata: attribute layout, varying layout, uniform layout, texture slots,
  VPM extents, register count used, program length;
* a **checker report** (§5.1) that the program satisfies every restriction.

### 4.10 The emitted subset has no addressing capability

This is a security requirement, not a style rule, and it belongs to the backend
because only the backend knows what it emits.

The QPU gives a shader routes to memory: the VPM's general-purpose DMA mode
writes to an address held in registers, and a direct-addressed TMU lookup reads
from an address supplied in the uniform stream. Both are legitimate hardware
features and both are privilege escalations if an untrusted shader can use them.
The design closes them by **never emitting them**:

| Rule | How the backend complies |
|---|---|
| No VPM general-purpose DMA | Every write to the VPM configuration and address registers uses an **immediate constant** whose mode selects register-file-to-VPM transfer. The backend never computes those values at run time, never derives them from a uniform, and never emits a variant that does. |
| No direct-addressed TMU lookup | The sample metadata the backend emits declares descriptor-based addressing only. The driver refuses a program whose metadata declares direct addressing. |
| No other memory access | The only effects the IR has are texture fetch, tile-buffer read, tile-buffer write, varying read and output write; the backend lowers them and nothing else. |

The checker enforces the first rule by rejecting a computed write to the
configuration registers, and the driver enforces the second by refusing the
metadata. Neither is a dataflow proof, which is why §9 records the residual
uncertainty and why the subset must stay small.

The capability record declares `addressing: none`, so the front end and the
kernel both know that a program from this backend is expected to satisfy the
subset — and a program that does not is refused rather than run.

---

## 5. Testing without hardware

Three mechanisms, in increasing cost, plus a board run for what none of them can
reach.

### 5.1 The checker pass (production)

A pass that runs on every compiled program, in the shipping configuration,
because it is the same check the kernel performs at `create_program`
(`docs/QUICKDRAW-3D-DESIGN.md` §5.2). It verifies:

* every instruction is a legal encoding;
* every restriction in `docs/QPU-RESEARCH.md` §9 holds, with the offending
  instruction index when one does not;
* the subset rules of §4.10 hold: every write to the VPM configuration and
  address registers is an immediate selecting register-file transfer, and no
  metadata declares direct TMU addressing;
* every branch target is inside the program;
* delay slots are filled and legal;
* the metadata is internally consistent (VPM extents, attribute and varying
  counts, uniform size, texture slots);
* the program length is within the instruction memory.

Because it is production code, it cannot be skipped and it cannot rot: a
miscompile that produces an illegal program is caught before submission.

### 5.2 The QPU interpreter (development tool)

An executable model of the 16-wide QPU: the register file, the two ALUs, pack
and unpack, the VPM, a modelled TMU with a simple texture format, the uniform
stream, and the tile buffer. It executes the machine code and produces the
outputs the shader writes.

It is used to compare against a reference interpretation of the IR (a second,
deliberately naive evaluator) on a corpus of programs. A disagreement is a
miscompile. The interpreter is not a hardware model: it does not model latency,
bank conflicts, denormal behaviour or TMU filtering, so it cannot establish that
the hardware does what the program says — only that the program does what the
source says.

### 5.3 Golden encodings (development tool)

A corpus of small programs with the expected 64-bit words, checked exactly.
This is what catches an encoding regression, and it doubles as executable
documentation of the ISA: each case is one instruction or one short sequence,
with the field values it is meant to encode.

The encodings can be cross-checked against an independent assembler's output for
the same instruction. That is a comparison of *facts* (does this mnemonic with
these operands produce this word), not a use of another project's code, and it is
consistent with the clean-room rule in `docs/QPU-RESEARCH.md` §1.3.

### 5.4 Differential testing (development tool)

Generate random IR programs within the target's capability set, compile them,
run the interpreter and the IR reference evaluator, and compare. This is the
cheapest way to find the interaction bugs — a scheduler that reorders a
dependent pair, an allocator that reuses a live register, a pack/unpack that
loses a lane — that a hand-written corpus misses.

### 5.5 What only a board can establish

* TMU latency and whether the scheduling gaps are sufficient.
* VPM bank conflicts and whether the allocation strategy is efficient.
* Denormal and rounding behaviour of the SFU and of `fmul`.
* Whether the restriction list is complete (an adversarial program per rule).
* Whether the program actually produces the pixels the corpus expects, on
  silicon, in the shipping configuration.

### 5.6 How this relates to the recovery order

The project's current instruction removes the Rust test suite and defers the
design of new validation machinery until the user agrees on it. The mechanisms
above are therefore specified as **development tools and a production pass**, not
as a revived unit-test framework:

* the checker is production code, always on;
* the interpreter, the golden corpus and the differential generator are a
  command-line development tool, run by a developer, with results recorded as
  evidence rather than as pass counts;
* no `#[test]` harness is proposed, and none should be added without the user's
  agreement, per the recovery order.

---

## 6. How a backend declares what it supports

Each backend exposes a **capability record**, and the front end consults it
before lowering:

| Field | Example (QPU) | Why the front end needs it |
|---|---|---|
| `scalar_types` | `i32, u32, f32, bool` | Reject `i64` at parse time with a named reason. |
| `vector_widths` | 2, 3, 4 | Reject a `vec8`. |
| `packed_types` | `u8x4` | Decide whether a packed operation is native or lowered. |
| `integer_multiply` | 24-bit native; 32-bit by synthesis, disabled by default | Reject a 32-bit multiply the target cannot do. |
| `divergence` | none | Reject non-uniform control flow. |
| `loops` | uniform trip count or uniform exit | Reject a data-dependent loop. |
| `calls` | inlined only | Reject recursion at parse time. |
| `memory` | textures and tile buffer only | Reject any other memory. |
| `addressing` | none: no VPM general-purpose DMA, no direct-addressed TMU | The kernel's subset check and the driver's metadata refusal depend on this being declared and enforced. |
| `registers` | 5 accumulators + 64 A/B (16 A/B when threaded) | Reject a shader whose pressure cannot be allocated, with the pressure reported. |
| `vpm_bytes` | from `V3D_IDENT1`, partitioned by `VPMBASE` | Reject a varying/attribute layout that does not fit. |
| `uniform_bytes` | bounded by the stream budget | Reject an oversized uniform block. |
| `texture_formats` | RGBA8888, RGB565, luminance, alpha | Reject an unsupported sample format. |
| `derivatives` | none | Reject `dFdx`/`dFdy`. |
| `discard` | none | Reject `discard`. |
| `float_semantics` | IEEE binary32, denormals may flush | Let the front end warn where it matters. |
| `branch_restrictions` | the §9 list | Drive the checker. |
| `max_program_length` | target-specific | Reject an oversized program. |

The record is data, not code, so the front end's rejection logic is written once
and works for every backend. A new backend is a new record plus a lowering; the
front end does not change. That is the test of whether the IR is genuinely
target-neutral: adding V3D 4.x must not touch the parser, the type checker or
the IR verifier.

---

## 7. A second target

V3D 4.x and V3D 7.x are different GPUs, not revisions of VC4:

* a different QPU instruction encoding and register model;
* an MMU, so the submission path's no-IOMMU argument does not carry over;
* a different control-list record set, described per generation.

What plugs in:

* a new backend crate implementing the same trait: lower IR → target IR →
  allocate → schedule → encode → metadata;
* a new capability record;
* no change to the front end, the IR, or the QuickDraw and 3D front ends above.

What does not carry over: the VC4-specific lowering strategies (delay slots,
`TMU_NOSWAP`, the `r4` rules, `VPMBASE`), which live in the VC4 backend and
should not be generalised into the IR.

The honest limit: V3D 4.1 has a published guide, V3D 7.x does not in the same
form, and Mesa's compiler is the reference for both. The second backend is
therefore a research task as well as an engineering one, and its capability
record is where that uncertainty is made explicit.

---

## 8. Package layout

```
crates/
  mac-shader/                  facade for applications: compile(source, target) -> Program
  mac-shader-syntax/           lexer, parser, AST; no_std
  mac-shader-hir/              name resolution, type checking, HIR; no_std
  mac-shader-ir/               target-neutral SSA IR, builder, verifier; no_std
  mac-shader-opt/              target-independent passes: const fold, DCE, CSE,
                               inlining, structured-CF simplification; no_std
  mac-shader-target/           Backend trait, capability record, metadata types,
                               shared lowering helpers; no_std
  mac-shader-qpu/              VC4 QPU backend: lowering, VPM/uniform layout,
                               register allocation, scheduling, encoding; no_std
  mac-shader-qpu-isa/          VC4 instruction definitions, assembler and
                               disassembler, the restriction checker; no_std
  mac-shader-v3d/              (later) V3D 4.x/7 backend; no_std
  mac-shader-tools/            host-only development tool: interpreter, golden
                               corpus runner, differential generator, disassembly
                               dump. Not linked into the OS.
```

Notes:

* The compiler core is `no_std`, because QuickDraw's shaders are compiled where
  they are used and because a future in-System compile must not depend on `std`.
  `mac-shader-tools` is the only crate that needs `std`, and it is never part of
  an OS image.
* `mac-shader-qpu-isa` is separate from the backend because the *checker* is
  production code used by both the compiler and the kernel's admission path. One
  definition of the ISA, one checker, two consumers.
* The metadata types live in `mac-shader-target`, shared by the compiler, the
  client library and the kernel service, so that the shader ABI has one
  definition.

### 8.1 Determinism

The same source and the same capability record must produce the same machine
code, byte for byte, on every build. This is a requirement, not a nicety: the
golden encodings are only meaningful if the output is deterministic, and a
future reproducibility check depends on it. Sources of nondeterminism to exclude:
hash-map iteration order, allocator addresses, and any dependence on wall-clock
time.

---

## 9. What is not established

1. **Whether the language is the right size.** It is designed for the two known
   consumers. A third consumer may need something it lacks, and the answer must
   be a language change with a capability record, not a target-specific hack.
2. **The register-pressure threshold at which a real shader must be split or
   refused.** No shader has been compiled yet.
3. **Whether the interpreter's model is faithful enough to catch the bugs that
   matter.** It cannot model latency; the scheduler's correctness depends on
   latency assumptions it does not check.
4. **The actual VPM budget under a chosen `VPMBASE` partition.**
5. **Whether `imul24`-based synthesis is fast enough** for any shader that needs
   a 32-bit multiply.
6. **Whether the restriction list is complete.** An undocumented rule would make
   the checker incomplete and the golden corpus insufficient.
7. **Whether the no-addressing subset is closed.** The design closes the VPM
   general-purpose-DMA route and the direct-TMU route. Whether a third route
   exists — an instruction sequence that reaches memory without either — is not
   established, and it is the one open question that could turn a wrong picture
   into memory corruption. Resolution: adversarial programs on the board, one
   per suspected route.
7. **The cost of a compile.** If QuickDraw shaders are compiled at start-up, the
   compile time is a boot-time cost; if they are precompiled into the image, the
   determinism requirement (§8.1) becomes load-bearing. Not measured.

---

## 10. Board runs this design needs

1. A minimal vertex/fragment pair, compiled, run, and read back — the first
   proof that the encoder, the metadata and the driver's shader record agree.
2. A QuickDraw rectangle-fill shader, compared byte-for-byte with software.
3. A packed 8-bit blit shader, to exercise `u8x4` and the pack/unpack encoding.
4. A shader that reads a texture, to exercise the uniform stream, the texture
   descriptor and TMU scheduling.
5. A shader that violates each restriction in turn, to confirm the checker
   matches silicon.
6. A shader that exceeds the register file, to confirm the refusal path.
7. A shader with a loop, to confirm the uniform-trip-count rule and the branch
   delay-slot filling.
8. A program per escape route — a computed write to the VPM configuration
   register, and a sample declaration with direct addressing — to confirm both
   are refused. This is the test that matters most for memory safety, and it
   must be run on silicon, not only against the checker.

Everything in §5.1–§5.4 runs without hardware and without QEMU: the checker, the
interpreter, the golden encodings and the differential generator are host tools
over machine code. They establish that the compiler emits the program it means
to emit, and nothing about what the silicon does with it. §5.5 is the board
list, and `docs/QPU-RESEARCH.md` §3.5 records why QEMU cannot substitute for it.

---

## 11. Decisions a reviewer must make

Identifiers are global and defined once, in `docs/QPU-RESEARCH.md` §16.1. This
document originates these:

| Id | Decision | Argued in |
|---|---|---|
| **D25** | The no-divergence rule. | §2.3, §4.1 |
| **D26** | The checker is production code, run on every compile. | §5.1 |
| **D27** | The development-tool form of the interpreter and corpus, not a `#[test]` framework. | §5.2–5.6 |
| **D28** | The crate split, with `mac-shader-qpu-isa` as the single ISA definition. | §8 |

D8 (purpose-built language), D7 (register exhaustion is a refusal) and D12
(precompiled shaders versus compile at start-up) are argued here and defined in
the consolidated list. D2 — the no-addressing subset — is the backend rule this
document implements in §4.10.
