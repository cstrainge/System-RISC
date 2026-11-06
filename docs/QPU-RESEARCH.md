# The VideoCore IV 3D block and QPU: sources, licences and extracted interface facts

Status: research record, not a design. This document states what the 3D block
is, what it accepts, and where each fact came from, so that the four design
documents that follow it — and any later lane that reads only the repository —
do not have to re-derive it or guess.

It extends `docs/VIDEOCORE-2D-DESIGN.md` §7 rather than repeating it. That
section establishes reachability: the V3D is at bus `0x7EC00000` — ARM
`0x3FC00000`; §2.7 corrects the ARM figure that section states — its clock comes
from the clock manager and its reset from the power-management block, it raises
GPU interrupt 10, and it is therefore tier 1 (direct register access from the
ARM), not tier 3. Nothing here re-opens that. This document is about what the
block *does* once reached.

Nothing in this document is code. Facts are stated in prose and tables, and no
source's identifiers, comments or structure are reproduced. That is deliberate:
see §1.3.

---

## 1. Method

### 1.1 What counts as evidence

Three classes of source, in descending authority:

1. **Primary documentation.** The *VideoCore IV 3D Architecture Reference
   Guide*, `VideoCoreIV-AG100-R`, Broadcom, 16 September 2013. This is the
   authority on the interface. Where it is silent, the silence is recorded
   rather than filled.
2. **Working implementations**, read for what they actually write and in what
   order: Mesa's `vc4` Gallium driver, the Linux `drm/vc4` kernel driver, and
   the `py-videocore` QPU assembler/driver. These are cited for facts about
   behaviour on this silicon, not for structure.
3. **Device tree and board records**, for addresses, interrupt numbers, clock
   and reset wiring.

A fact is only recorded as established if it is stated by a source above and is
consistent with at least one independent implementation, or is directly readable
from a register/encoding definition that two implementations agree on. Anything
weaker is marked as inference or as unestablished in §12.

### 1.2 Scope of the hardware

The BCM2837-class SoC carries a V3D 2.1-class 3D pipeline: a tile
binner, primitive setup, and a shader core built from quad processors (QPUs).
`py-videocore` states the part has twelve QPUs; the guide's model is four QPUs
and one texture unit per slice. The *actual* slice, QPU, TMU, semaphore and VPM
counts are reported by `V3D_IDENT1` and must be read at bring-up rather than
assumed (§3.2). This document uses "twelve" only as the expected value for this
board.

The block is a **bus master**. It reads control lists, shader programs, vertex
data, textures and binner state directly from RAM, and writes render targets and
binner output directly to RAM. There is no IOMMU between it and system memory on
this part. That single fact drives the entire submission design
(`docs/GPU-SUBSYSTEM.md` §6).

### 1.3 Licensing discipline

The project is clean-room. The rule applied here is:

* read any source, record the fact and where it came from;
* never copy code, identifiers, comments or control flow into this repository;
* do not paste source into the repository at all, even as a quotation.

| Source | Licence | What that permits here |
|---|---|---|
| *VideoCore IV 3D Architecture Reference Guide* | Broadcom published reference document; no open licence grant. Treated as a specification to be read. | Facts and interface semantics only. No text reproduced. |
| Mesa `src/gallium/drivers/vc4/*` | MIT (Broadcom copyright headers on the files read; Mesa's licence is MIT) | May be studied freely. Still not copied: the design is derived from this project's module model, and a copied CL builder would import Mesa's structure and its bugs. |
| Mesa `src/gallium/drivers/vc4/kernel/vc4_packet.h` | MIT (Broadcom header) | Encoding facts. The packet table in §4 is written from the facts, not transcribed. |
| Linux `drivers/gpu/drm/vc4/vc4_validate.c` | MIT (Broadcom header, `Copyright © 2014 Broadcom`) | Facts about the validation model and the structures it checks. |
| Linux `drivers/gpu/drm/vc4/vc4_v3d.c`, `vc4_regs.h` | GPL-2.0-only | Facts only. Register offsets and bit positions are interface facts; no code, no field-name tables, no macros. |
| Linux `arch/arm/boot/dts/broadcom/*.dtsi` | GPL-2.0 | Addresses, interrupt numbers, clock/reset wiring — facts. |
| `py-videocore` (Idein) | MIT | Independent confirmation of QPU encoding, the small-immediate table, the SFU semantics, and the mailbox QPU-execution model. |
| Mesa `src/gallium/drivers/v3d/*`, `src/broadcom/*` (second target) | MIT | Facts about V3D 4.x/7 for the second compiler backend. |
| Linux `drivers/gpu/drm/v3d/*` | GPL-2.0 | Facts only. |

Where a fact is available only from a GPL source, it is recorded here as a fact
with the file cited, and nothing from that file is reproduced. No fact in this
document depends on a source whose licence is unclear.

---

## 2. Source inventory

### 2.1 Primary documentation

**`VideoCoreIV-AG100-R`, *VideoCore IV 3D Architecture Reference Guide*,
Broadcom, 16 September 2013.** Sections used:

* §3 — QPU register map and instruction set: the 64-bit instruction word, the
  two ALUs, the register files, the signal field, pack/unpack, branches, load
  immediate, semaphores.
* §4–§8 — the fixed-function pipeline: primitive setup, clipping, the tile
  buffer, texture unit.
* §9 — control lists: record formats and identifiers, the binner, tile memory.
* Appendix B, Table 95 — V3D register base for BCM2835 at `0x7ec00000`.
* The errata table — QPU and tile-buffer restrictions.

Two errata are known to affect this work and are recorded in §9.4: the guide's
Table 8 is mislabelled, and three of its R4-unpack rows are wrong. Both are
corroborated by an independent assembler's errata list, and both are consistent
with the encoding that Mesa and that assembler actually emit. Where the guide
and the encoders disagree, **the encoders are believed**, because they are
executed against the silicon.

### 2.2 Mesa `vc4` — the closest thing to a complete reference implementation

Files read, with the specific facts taken:

| File | Facts taken |
|---|---|
| `kernel/vc4_packet.h` | Packet identifiers, packet sizes, tile-buffer size, tiling formats, load/store buffer selectors, render-config fields, binner-config fields, texture descriptor field layout, texture type enumeration. |
| `vc4_draw.c` | Binning sequence order: tile-binning mode config, start tile binning, primitive-list format, shader state, attribute records, primitive, flush. Shader-record sizes and the 8-attribute limit. The 65 535-vertex split. |
| `vc4_emit.c` | Per-state packet emission into the bin list: clip window, configuration bits, clipper scaling, viewport offset, flat-shade flags. Guardband clipping: primitives rasterise outside the view volume, so the clip window must be set to the viewport. |
| `vc4_job.c` | The job is four separate streams — bin CL, shader records, uniforms, BO handles — submitted together; surfaces (colour read/write, Z read/write, MSAA) are described separately to the kernel; tile size 64×64, or 32×32 with 4× MSAA. |
| `vc4_qpu_defines.h` | The complete 64-bit instruction field layout, signal encodings, add/mul opcodes, read and write address spaces, branch encoding, load-immediate modes, pack/unpack encodings. |
| `vc4_qpu_validate.c` | The guide's "Summary of Instruction Restrictions" (page 37) restated as executable checks — the single most useful artefact in the tree for a compiler backend. |
| `vc4_register_allocate.c` | Register model: five accumulators plus 64 A/B registers; two register classes for threaded fragment shaders (only the bottom 16 A/B registers are available when two threads share the file); r14 reserved for a read-address-conflict fixup that the emitter performs; no memory spilling. |
| `vc4_rcl` behaviour (in `vc4_job.c`/`vc4_context.c`) | The render list is a control list executed per tile: tile coordinates, tile-buffer load, a branch into that tile's bin list, tile-buffer store. |

Mesa's `vc4` driver is MIT and may be studied without restriction. It is *not*
used as the architecture: the control-list builder, the object model and the
submission path in this project are derived from this project's own driver and
service rules, and Mesa's are cited only as evidence of what the hardware
accepts.

### 2.3 Linux `drm/vc4` — the submission and validation model

| File | Facts taken |
|---|---|
| `vc4_validate.c` | The block's own comment states the threat model: no IOMMU, so a user with control-list access could overwrite or read system memory. The kernel therefore whitelists packet types, checks that every address field lies inside a buffer object the job declared, and rewrites the list into a validated copy with real device addresses (relocation). It also validates binner configuration, tile-state sizing, shader-record alignment, index-buffer bounds, and every texture descriptor found in the uniform stream, including UBO clamp bounds and mip-chain size. |
| `vc4_validate_shaders.c` | Shader programs are validated separately, instruction by instruction. Its file comment states the shader threat model exactly: without an IOMMU, a user who can execute shaders could escalate privilege by **overwriting system memory through the VPM write-address register in general-purpose DMA mode**, or read memory it should not **by reading it as a texture, as uniform data, or through a direct-addressed TMU lookup**. The validator therefore walks the shader's instructions, bounds its accesses, and records where texture accesses are made so the uniform stream can be relocated. It is performed once, at shader creation, because shader buffers are immutable for their lifetime. |
| `vc4_v3d.c` | Bring-up: enable clock, release reset, read `V3D_IDENT0` and compare with the expected value, install the interrupt, disable the binner-overflow address at setup and teardown. `V3D_VPMBASE` is set to 0, which gives the whole VPM to the fixed-function pipeline and none to standalone QPU programs. |
| `vc4_regs.h` | Register offsets and bit positions: identification, interrupt control/status, control-list state and addresses, binner registers, QPU scheduler registers, VPM base, performance counters. |

`vc4_validate.c` is MIT; `vc4_v3d.c` and `vc4_regs.h` are GPL-2.0 and are cited
for facts only.

The important negative finding from this file set is in §6.2: the validator is
not a general-purpose safety proof. It is sound because it accepts a narrow
whitelist of packets and checks each field, and because shaders are validated
separately. Any design that accepts *arbitrary* lists inherits that whole
surface.

### 2.4 `py-videocore` — independent confirmation, and the direct QPU path

`py-videocore` (Idein, MIT) is a GPGPU library for the same part. It contains a
QPU assembler, an instruction encoder, and a driver that executes QPU programs
through the firmware.

Facts taken:

* **Independent confirmation of the instruction encoding** — the same field
  widths and positions as §8.2, arrived at from the guide rather than from Mesa.
* **The small-immediate table** in full: 0…15, −16…−1, 2⁰…2⁷, 2⁻⁸…2⁻¹, with 48
  meaning "rotate by r5" and 49…63 meaning "rotate by n channels".
* **A semaphore instruction exists in the ISA**: a load-immediate-signal
  instruction with mode 4 carries a 4-bit semaphore number and an acquire/release
  bit, so QPU threads can synchronise without the fixed-function pipeline.
* **SFU semantics**: log and exp are base 2; reciprocal of zero is infinity;
  reciprocal square root is `1/sqrt(abs(x))`; log is `log(abs(x))`.
* **Errata of the guide**: Table 8 is the R4 *unpack* table, and its rows for
  8b/8c/8d repeat 8a incorrectly.
* **The firmware QPU-execution model** (the path this project will *not* use):
  the ARM asks the firmware, over the mailbox, to run *n* QPUs, passing a
  per-thread message block containing a uniform pointer and a program address;
  the firmware enables the QPU, runs the programs, and returns a timeout status.
  Memory comes from the VideoCore shared-memory allocator, and the caller must
  clean before and invalidate after because the QPU is another bus master.
* **`enable_qpu`** is a mailbox operation; the firmware must have the QPU
  enabled for this path to work at all.

That last point matters for the design: the widely available "run a QPU program"
recipes on this platform go *through the firmware*, which is a tier-3-style
service. This project does not need it — the control-list path is tier 1 — but
it explains why so much public QPU material assumes firmware involvement, and
why the firmware's VPM/queue configuration must not be assumed to match a
bare-metal control-list submission.

### 2.5 Device tree

`arch/arm/boot/dts/broadcom/bcm2835-common.dtsi`:

* `v3d@7ec00000`, `reg = <0x7ec00000 0x1000>`, `interrupts = <1 10>`,
  `resets = <&pm BCM2835_RESET_V3D>`.
* The `pm` node (`watchdog@7e100000`, `brcm,bcm2835-pm`) lists the clocks it
  supplies, including `BCM2835_CLOCK_V3D`.
* The DMA controller declares `brcm,dma-channel-mask = <0x7f35>` — the channels
  the firmware leaves to the ARM.

Consequences: the V3D window is one 4 KiB page; the interrupt is GPU interrupt
10 (bank 1, index 10); the reset line belongs to the PM block; and the V3D node
itself declares **no** `clocks` property — the V3D clock is listed by the PM
node. A driver must therefore obtain the clock by name from a clock service and
must not assume the device node hands it one. Linux treats the clock as optional
and relies on PM runtime; this project's driver must treat its absence as a hard
failure, because it cannot program the block without it.

### 2.6 Second-target material (V3D 4.x / 7)

* Mesa `src/gallium/drivers/v3d/*` and the shared `src/broadcom/*` compiler —
  MIT. Per-generation control-list packet descriptions live in the shared
  Broadcom directory, not in the driver directory.
* Linux `drivers/gpu/drm/v3d/*` — GPL-2.0, facts only. The driver has a separate
  MMU module and GPU-virtual-address allocation, which is the structural
  difference from VC4: **V3D 4.x and later have an MMU; VC4 does not.**
* V3D 4.1 has a published architecture guide. V3D 7.1 (the later SoC generation) is not publicly
  documented in the same form, and Mesa's compiler is the de facto reference.

No V3D 4.x/7 fact is used by the first landing. §10 records what the second
backend will need and what is not established.

### 2.7 Sources that disagree, and which is believed

Several sources disagree about the V3D, including two of this project's own
documents, and a later lane reading only one of them would reach the opposite
conclusion. This is recorded here because the disagreement is the single most
consequential provenance fact in this document.

**Conflict 1 — the V3D's ARM address.**

The translation rule on a 1 GiB part is
`ARM = bus - 0x7E000000 + 0x3F000000`. Calibrate it on two points the record
already agrees on, then apply it to the third:

| Device | Bus address | Arithmetic | ARM address |
|---|---|---|---|
| RNG | `0x7E104000` | `0x3F000000 + 0x00104000` | `0x3F104000` ✓ |
| HVS | `0x7E400000` | `0x3F000000 + 0x00400000` | `0x3F400000` ✓ |
| **V3D** | **`0x7EC00000`** | **`0x3F000000 + 0x00C00000`** | **`0x3FC00000`** |

The converse check is decisive: `0x3FEC0000` would be bus `0x7EEC0000`, which no
source gives for anything.

| Source | Claim | Verdict |
|---|---|---|
| the archived BCM2837 hardware inventory, row "V3D (3D engine)" | Base `0x3FC00000`, 64 KiB, interrupts 66 and 67, category **VideoCore** | **Address correct**; size, interrupt count and category wrong (below) |
| `docs/VIDEOCORE-2D-DESIGN.md` §7 | Bus `0x7EC00000` / ARM `0x3FEC0000` | Bus correct; **ARM address wrong** — the offset `0x00C00000` was written as `0x00EC0000` |
| Linux device tree, `bcm2835-common.dtsi` | `v3d@7ec00000`, `reg = <0x7ec00000 0x1000>` | Consistent with the rule → ARM `0x3FC00000` |
| QEMU `include/hw/arm/raspi_platform.h` | `V3D_OFFSET 0xc00000` from the peripheral base | `0x3F000000 + 0x00C00000` = `0x3FC00000` — **the same address as the silicon** |

So the earlier claim, repeated in §2.7's first draft and in the 2D design's §10,
that QEMU places the V3D "at `0x3FC00000` rather than the silicon's
`0x3FEC0000`", is itself wrong. QEMU's offset is the correct bus offset; what
was wrong was the ARM translation in the design document. This document's first
draft repeated that translation and asserted the inventory was wrong. **That is
withdrawn**: the inventory's address is right, and the design document and this
one were wrong. It is recorded rather than silently corrected because the whole
purpose of this section is to stop the next lane from re-deriving it differently
again.

**Conflict 1b — the interrupt number.**

The device tree property is `interrupts = <1 10>` under
`brcm,bcm2835-armctrl-ic`. That binding has two cells: the bank, then the bit
within the bank. Bank 0 is the "IRQ basic pending" register, bank 1 is "IRQ
pending 1", bank 2 is "IRQ pending 2", and the binding enumerates the sources by
name. It lists **bank 1, bit 10 as `VC_3D`** — the 3D block's interrupt — and
bank 2, bit 1 as `VIDEOSCALER`.

Calibrate the encoding against sources that already agree:

| Property | Encoding | Meaning | Cross-check |
|---|---|---|---|
| system timer | `<1 0>`–`<1 3>` | GPU IRQs 0–3 | The timer's four compare channels |
| GPIO | `<2 17>`–`<2 20>` | GPU IRQs 49–52 | Inventory: GPIO interrupts 49–52 |
| UART0 | `<2 25>` | GPU IRQ 57 | Inventory: PL011 is 57 |
| SPI0 | `<2 22>` | GPU IRQ 54 | Inventory: SPI0 is 54 |
| sdhci | `<2 30>` | GPU IRQ 62 | Inventory: sdhci is 62 |
| mailbox | `<0 1>` | basic pending bit 1 | The binding's `ARM_MAILBOX` |

So `<1 10>` is **GPU interrupt 10**, and QEMU's header independently defines
`INTERRUPT_3D 10`. The inventory's "66, 67" cannot be right: bank 2's bit field
is 0–31, so bits 34 and 35 do not exist, and GPU IRQs stop at 63. The "three
interrupt lines (job done, MMU error, flo)" is also wrong: there is **one** ARM
interrupt line for the block. The *internal* status register has four bits
(`FRDONE`, `FLDONE`, `OUTOMEM`, `SPILLUSE`), which is a different thing and is
probably the origin of the "three lines" claim.

**Conflict 1c — the window size.** The device tree gives `0x1000`; the register
map in §3.3 uses nothing above `0xF20`. One page is the window on this part.
64 KiB is not this block's window.

**Conflict 1d — reachability and category.** The inventory classifies the block
as **VideoCore** and says "the ARM has no supported way to submit jobs".
`vc4_v3d.c` does exactly that from the ARM: clock, reset, identity read, control
lists, interrupt. The category should be **ARM** (a driver is possible), with
the firmware's 3D stack noted as the owner of the *current* boot path rather
than as an access restriction.

**Consequences, and one thing to check before the record is written.**

* The inventory's **address stands**. Its size, interrupt count, category and
  reachability claim need correcting.
* The 2D design's ARM address is wrong and should be corrected where that
  document lives; it is not on this branch, so this document records the finding
  and does not edit it.
* The window the platform lane is adding is described as "bus `0x7EC00000` /
  ARM `0x3FEC0000`". **That pair is internally inconsistent**: by the derivation
  above, bus `0x7EC00000` is ARM `0x3FC00000`, and ARM `0x3FEC0000` is bus
  `0x7EEC0000`. If the record stores the ARM address, `0x3FEC0000` is the wrong
  page — a driver would probe `0x3FEC0000`, which is bus `0x7EEC0000`, and find
  nothing. This is the one item in this section that could cause a bring-up
  failure rather than a documentation error, and it should be checked before the
  record is committed.

**Conflict 2 — "Mesa `vc4` is not available on a 64-bit build of the board".** A web search
returned this claim, attributed to 64-bit images for the board. It is not
evidence about the hardware: the Mesa driver is architecture-independent C, and
the register interface does not change with the ARM's exception level or
register width. What a distribution ships in a 64-bit kernel configuration is a
packaging decision. The claim is recorded because it is the kind of statement a
later lane might treat as a hardware limit, and it is **not** used here. Why the
reported behaviour occurs was not established, and it does not matter: this
design programs the block directly and does not use that driver stack.

**Conflict 3 — the guide's Table 8.** The guide mislabels the R4 pack/unpack
table and gives three wrong rows; an independent assembler's errata says the
same, and the encoders both emit the corrected values. **Believed: the
encoders**, because they execute against the silicon. See §9.4.

**Conflict 4 — the "supported" QPU path.** Public QPU material runs programs
through a firmware mailbox service; Linux runs control lists directly. These are
not contradictory facts but different paths, and this design uses the direct one
because it is tier 1 and because Linux demonstrates it. The firmware path's
existence is not evidence that the direct path is unavailable.

**Conflict 5 — the V3D's clock.** The `v3d` device-tree node declares **no**
`clocks` property; the PM node lists `BCM2835_CLOCK_V3D`. A reader could
conclude the block has no clock control. It does: the clock manager generates
it, the PM block gates and powers it, and the node expresses that through the
reset reference and the PM node's clock list. Linux treats the driver's clock as
optional because PM runtime covers it; this design requires it, because a driver
that cannot enable the clock cannot trust any register read.

---

## 3. The block: address map and bring-up

### 3.1 Window and identity

| Item | Value | Source |
|---|---|---|
| Bus base | `0x7EC00000` | Guide Appendix B; device tree `v3d@7ec00000` |
| ARM physical base | `0x3FC00000` | Derived: `0x3F000000 + (0x7EC00000 - 0x7E000000)`; calibration in §2.7 |
| Peripheral aperture (ARM) | `0x3F000000`, 16 MiB | Inventory; the base the derivation above uses |
| Peripheral bus base | `0x7E000000` | Guide §1.2.4 |
| Window size | `0x1000` (one page) | Device tree `reg = <0x7ec00000 0x1000>` |
| ARM interrupt | GPU interrupt 10 | Device tree `interrupts = <1 10>`; bank 1 bit 10 is `VC_3D` in the binding (§2.7) |
| Clock | clock manager's V3D clock | Device tree (PM node lists it) |
| Reset | PM block's V3D reset | Device tree `resets = <&pm BCM2835_RESET_V3D>` |

### 3.2 Identification registers

| Offset | Register | Content |
|---|---|---|
| `0x00000` | `V3D_IDENT0` | Expected value `0x02443356` (revision 2, "V3D"). |
| `0x00004` | `V3D_IDENT1` | VPM size in KiB (bits 31:28), semaphore count (23:16), TMUs per slice (15:12), QPUs per slice (11:8), slice count (7:4), revision (3:0). |
| `0x00008` | `V3D_IDENT2` | Implementation-specific. |

`0x02443356` is the expected `V3D_IDENT0` for this class of part and is what the
Linux driver compares against. Reading it is the cheapest proof that the block
is alive after clock enable and reset release, and it is the first stage of the
bring-up contract in `docs/GPU-SUBSYSTEM.md` §3.

The counts in `V3D_IDENT1` are **derived**, not assumed: the number of QPUs is
slices × QPUs-per-slice, the number of TMUs is slices × TMUs-per-slice, and the
semaphore count is a field. A driver that hardcodes twelve QPUs, or sixteen
semaphores, is relying on this board rather than on the register, and will be
wrong on the next part.

### 3.3 Register map

Offsets from the window base. This is the set a driver actually touches; it is
not the complete map. **Provenance:** every offset and bit position below was
checked against Linux `drivers/gpu/drm/vc4/vc4_regs.h` (GPL-2.0, cited for facts
only) and cross-checked against the guide's §3 register map. Where the header
names a register but its semantics were not extracted, the row says so rather
than guessing — a guessed field is worse than a blank one, because a driver
written from it would be wrong in a way no test detects until it is on silicon.

| Offset | Register | Use |
|---|---|---|
| `0x00020` | `L2CACTL` | L2 cache clear (bit 2), disable (bit 1), enable (bit 0). |
| `0x00024` | `SLCACTL` | Slice cache control, per-TMU and uniform caches. |
| `0x00030` | `INTCTL` | Interrupt status / clear. |
| `0x00034` | `INTENA` | Interrupt enable. |
| `0x00038` | `INTDIS` | Interrupt disable. |
| `0x00100` / `0x00104` | `CT0CS` / `CT1CS` | Control-list status for list 0 (binning) and list 1 (rendering). |
| `0x00108` / `0x0010C` | `CT0EA` / `CT1EA` | Control-list end address. |
| `0x00110` / `0x00114` | `CT0CA` / `CT1CA` | Control-list current address; writing it starts execution. |
| `0x00118` / `0x0011C` | `CT00RA0` / `CT01RA0` | Return address after a sub-list branch. |
| `0x00120` / `0x00124` | `CT0LC` / `CT1LC` | Loop count. |
| `0x00128` / `0x0012C` | `CT0PC` / `CT1PC` | Program counter of the list processor. |
| `0x00130` | `PCS` | Pipeline state: binner out-of-memory (bit 8), render busy (3), render active (2), binner busy (1), binner active (0). |
| `0x00134` / `0x00138` | `BFC` / `RFC` | Binner and render flush counters. Named by the register header; their exact semantics were not extracted. |
| `0x00300` | `BPCA` | Binner address register. Named by the header; not interpreted here. |
| `0x00304` | `BPCS` | Binner status register. Named by the header; not interpreted here. |
| `0x00308` / `0x0030C` | `BPOA` / `BPOS` | Binner overflow address and size. |
| `0x00310` | `BXCF` | Binner extra configuration. Fields not extracted. |
| `0x00410` / `0x00414` | `SQRSV0` / `SQRSV1` | QPU scheduler reserved slots. |
| `0x00418` | `SQCNTL` | QPU scheduler control. |
| `0x00430` | `SRQPC` | Scheduler request queue: program counter. |
| `0x00434` | `SRQUA` | Scheduler request queue: uniforms address. |
| `0x00438` | `SRQUL` | Scheduler request queue: uniforms length. |
| `0x0043C` | `SRQCS` | Scheduler request queue: control/status. |
| `0x00500` | `VPACNTL` | VPM access control. |
| `0x00504` | `VPMBASE` | VPM partition: the boundary between the fixed-function pipeline's VPM and standalone QPU programs'. |
| `0x00670` / `0x00674` | `PCTRC` / `PCTRE` | Performance counter clear / enable. |
| `0x00680`+ | `PCTR(n)` / `PCTRS(n)` | Performance counter values and selectors. |
| `0x00F00`–`0x00F20` | `DBGE`, `FDBGO/B/R/S`, `ERRSTAT` | Debug and error status. |

The `SQ*` group is the **QPU scheduler queue**. It is the path by which the
fixed-function pipeline dispatches shader programs to QPUs, and by which a
driver can dispatch standalone QPU programs. `SRQPC` takes a program address,
`SRQUA`/`SRQUL` describe the uniform stream, and `SRQCS` reports queue state.
The exact field layout of `SRQCS` and `SQCNTL` was **not extracted** in this
pass; a driver must take it from the guide §3 and verify it on the board.

`VPMBASE` is load-bearing for the design: Linux writes zero, which reserves the
whole VPM for the fixed-function pipeline and leaves standalone QPU programs
with none. A design that wants both a 3D pipeline and standalone compute must
choose a partition, and that choice is global to the block, not per job. This
is recorded as a decision in `docs/GPU-SUBSYSTEM.md` §10.

### 3.4 Interrupt bits

| Bit | Name | Meaning |
|---|---|---|
| 0 | `FRDONE` | Render (list 1) finished. |
| 1 | `FLDONE` | Binning (list 0) finished. |
| 2 | `OUTOMEM` | Binner ran out of tile memory. |
| 3 | `SPILLUSE` | Binner spilled. |

The block raises one ARM interrupt (GPU 10) for all of them; the handler reads
`INTCTL` and clears the bits it handled. There is no per-context interrupt.

### 3.5 What QEMU can and cannot establish

The gate runs under QEMU's `raspi3b`, and the V3D there is an **unimplemented
placeholder at the correct address**. QEMU's peripheral container places the
device at `V3D_OFFSET 0xc00000` from the peripheral base, which is
`0x3FC00000` — the same address as the silicon (§2.7) — and an unimplemented
device answers reads with zero and discards writes.

That produces a clean split, and the design is written so the split is useful:

| Behaviour | Under QEMU | Why |
|---|---|---|
| Probing `0x3FC00000` | Reads return zero, so `V3D_IDENT0` ≠ `0x02443356` | Unimplemented device |
| The driver's refusal path | **Verifiable**: it must refuse to attach, report the capability absent, and leave the desktop on the software provider | This is exactly the path a board with a disabled or absent block takes |
| Register writes "sticking" | Not a test of anything | An unimplemented device discards writes; a window that falls through to RAM would echo them. Neither is evidence the block is present |
| Any control list, shader, tile or interrupt | **Not verifiable** | There is no model of the binner, the QPUs, the tile buffer or the interrupt |
| Identification from the identity register | **Not verifiable under QEMU, required on the board** | The only proof the block is alive |

So the parts of this design that QEMU can exercise are: the refusal and
degradation path, the object and handle bookkeeping, admission and accounting,
the scheduler's queue discipline, the span and request construction, and the
compiler's checker and encoder. The parts that need the board are: every
register interaction, every control list, every shader execution, the interrupt,
the reset, and every timing figure. A run under QEMU must not be reported as
evidence that the block works — only that the system behaves correctly when it
does not.

---

## 4. Control lists

A control list is a byte stream of records in memory. The list processor walks
it, feeding primitives and state changes to the fixed-function pipeline and the
shader core. Record 0 (`HALT`) stops the walk. The two list processors are
conventionally **list 0 = binning** and **list 1 = rendering**.

### 4.1 Record format

Every record begins with a one-byte identifier; the rest of the record is
fixed-size and follows immediately. Sizes below are from Mesa's packet header,
which matches the guide.

| Id | Record | Bytes | Purpose |
|---|---|---|---|
| 0 | `HALT` | 1 | Stop executing this list. |
| 1 | `NOP` | 1 | No effect. |
| 4 | `FLUSH` | 1 | Flush the pipeline; caps every bin list with a return. |
| 5 | `FLUSH_ALL` | 1 | Flush all state, including the shader core. |
| 6 | `START_TILE_BINNING` | 1 | Reset the binner's state-change counters; begin a binning pass. |
| 7 | `INCREMENT_SEMAPHORE` | 1 | Increment a semaphore; used to release the render list. |
| 8 | `WAIT_ON_SEMAPHORE` | 1 | Block until a semaphore reaches a value. |
| 16 | `BRANCH` | 5 | Absolute or relative branch, conditional. |
| 17 | `BRANCH_TO_SUB_LIST` | 5 | Call a sub-list, returning to the recorded return address. |
| 24 | `STORE_MS_TILE_BUFFER` | 1 | Store the multisample tile buffer. |
| 25 | `STORE_MS_TILE_BUFFER_AND_EOF` | 1 | As above, end of frame. |
| 26 | `STORE_FULL_RES_TILE_BUFFER` | 5 | Store the full-resolution tile buffer (address + flags). |
| 27 | `LOAD_FULL_RES_TILE_BUFFER` | 5 | Load the full-resolution tile buffer. |
| 28 | `STORE_TILE_BUFFER_GENERAL` | 7 | Store a selected buffer with format and tiling. |
| 29 | `LOAD_TILE_BUFFER_GENERAL` | 7 | Load a selected buffer with format and tiling. |
| 32 | `GL_INDEXED_PRIMITIVE` | 14 | Indexed primitive: mode, index type, count, index address, max index. |
| 33 | `GL_ARRAY_PRIMITIVE` | 10 | Array primitive: mode, count, first index. |
| 48 | `COMPRESSED_PRIMITIVE` | 1 | A primitive from the binner's compressed format. |
| 49 | `CLIPPED_COMPRESSED_PRIMITIVE` | 1 | As above, pre-clipped. |
| 56 | `PRIMITIVE_LIST_FORMAT` | 2 | Index width and primitive type for subsequent primitives. |
| 64 | `GL_SHADER_STATE` | 5 | Point at a shader record; attribute count. |
| 65 | `NV_SHADER_STATE` | 5 | Non-GL variant (clipping and point-size flags). |
| 66 | `VG_SHADER_STATE` | 5 | Vector-graphics variant. |
| 96 | `CONFIGURATION_BITS` | 4 | Rasteriser, depth and coverage configuration. |
| 97 | `FLAT_SHADE_FLAGS` | 5 | Which varyings are flat. |
| 98 | `POINT_SIZE` | 5 | Point size. |
| 99 | `LINE_WIDTH` | 5 | Line width. |
| 100 | `RHT_X_BOUNDARY` | 3 | Right-handed tile boundary. |
| 101 | `DEPTH_OFFSET` | 5 | Depth offset. |
| 102 | `CLIP_WINDOW` | 9 | Left, bottom, width, height in pixels. |
| 103 | `VIEWPORT_OFFSET` | 5 | Viewport centre in 1/16 pixel units. |
| 104 | `Z_CLIPPING` | 9 | Near/far clip planes. |
| 105 | `CLIPPER_XY_SCALING` | 9 | Viewport half-width/height in 1/16 pixel units. |
| 106 | `CLIPPER_Z_SCALING` | 9 | Z scale and offset. |
| 112 | `TILE_BINNING_MODE_CONFIG` | 16 | Tile alloc address and size, tile state address, tile counts, flags. |
| 113 | `TILE_RENDERING_MODE_CONFIG` | 11 | Tile buffer address, tile counts, format, tiling. |
| 114 | `CLEAR_COLORS` | 14 | Clear colour and depth values. |
| 115 | `TILE_COORDINATES` | 3 | Which tile the following load/branch/store applies to. |
| 254 | `GEM_HANDLES` | 9 | *Not a hardware record*: a Mesa convention for carrying buffer references in the stream so the kernel can relocate them. |

The last row is important as a warning: the packet numbering in Mesa's header
contains a **non-hardware** record at 254. It exists because Mesa builds a list
containing object references and lets the kernel substitute addresses. A design
that constructs lists itself has no need for such a record, and must not assume
every identifier in that table is hardware.

### 4.2 The binning pass

The bin list (list 0) is built as:

1. `TILE_BINNING_MODE_CONFIG` — tile state address, tile alloc address and
   size, tile counts in X and Y, and flags (auto-initialise tile state, block
   sizes, multisample mode, 64-bit tile buffers, non-MS depth buffer).
2. `START_TILE_BINNING` — resets the state-change counters. Those counters are
   what the binner uses, when a primitive lands in a tile, to decide which state
   records must be copied into that tile's list. This is why state must precede
   primitives.
3. `PRIMITIVE_LIST_FORMAT` — index width (8- or 16-bit) and primitive type.
4. State records for the draw (clip window, configuration bits, viewport,
   clipper scaling, flat shading, point size, line width).
5. `GL_SHADER_STATE` — the shader record, which names the vertex, coordinate and
   fragment shader addresses, the attribute arrays, and the varying counts.
6. `GL_INDEXED_PRIMITIVE` or `GL_ARRAY_PRIMITIVE`.
7. `INCREMENT_SEMAPHORE`, then `FLUSH`.

The binner writes, for each tile that receives geometry, a small list into the
tile alloc pool. `FLUSH` caps each of those lists with a return record, so that
when the render list calls one, control comes back to the render list. The
increment releases the render list, which has been waiting on the semaphore.

Two consequences the design must respect:

* **State precedes primitives, always.** A state record emitted after the
  primitive it was meant to affect has no effect on it.
* **`START_TILE_BINNING` must not appear twice** in one list, and
  `TILE_BINNING_MODE_CONFIG` must appear before it. The kernel validator enforces
  both; a list that violates them is malformed rather than merely wrong.

### 4.3 The render pass

The render list (list 1) is built as:

1. `TILE_RENDERING_MODE_CONFIG` — tile buffer address, tile counts, memory
   format, tiling, format, decimation, multisample mode.
2. `CLEAR_COLORS` — if the tile buffer is to be cleared rather than loaded.
3. For each tile, in order:
   * `TILE_COORDINATES` — which tile;
   * `LOAD_TILE_BUFFER_GENERAL` (or `LOAD_FULL_RES_TILE_BUFFER`) — load the
     previous contents if the tile is not being cleared;
   * `BRANCH_TO_SUB_LIST` — into that tile's bin list, which draws the geometry
     that landed in it;
   * `STORE_TILE_BUFFER_GENERAL` (or the full-resolution store) — write the tile
     back to the render target.
4. `FLUSH_ALL`, `HALT`.

The tile buffer is **64×64 pixels at 32 bits per pixel = 16 KiB** per buffer, or
**32×32 with 4× multisampling**. Colour and depth each need one. The tile buffer
is on-chip; that is the entire point of tiling, and it is why a full-screen
render costs one tile-buffer store per tile rather than one framebuffer pass.

### 4.4 Tile memory and the binner

The binner needs two regions in memory:

* **Tile state**: 48 bytes per tile, rounded up to a page. It holds, per tile,
  the head of that tile's list and its overflow pointer.
* **Tile alloc**: the pool the per-tile lists are allocated from, at 32 bytes
  per tile initially, plus overflow memory.

Both must be in the same buffer, because the tile state's pointers into the tile
alloc carry only 28 bits and take their top four bits from the tile state's own
address. The buffer therefore must not cross a 256 MiB boundary. Linux allocates
a 16 MiB pool, checks that it does not cross such a boundary, and carves it into
512 KiB slots, one per job. It also zeroes the tile-state array before each job,
because a stale tile state can be consumed by the primitive tile binner.

Linux rejects `DB_NON_MS` and `TILE_BUFFER_64BIT` binner flags; this design uses
neither. The maximum geometry it sizes for is 2048×2048 at 32×32 tiles = 4096
tiles, which is 192 KiB of tile state plus 128 KiB of tile alloc.

The binner can run out of tile memory. That raises `OUTOMEM` and sets
`PMS.BMOOM`. `BPOA`/`BPOS` describe an overflow region the binner may use. The
design treats `OUTOMEM` as a refusal cause, not as something to paper over: a
job whose geometry does not fit the tile pool must be split or refused.

### 4.5 Shader records

A `GL_SHADER_STATE` record carries an offset (in 16-byte units) into a separate
**shader record** array. Each shader record is:

* 36 bytes plus 8 bytes per attribute, or
* 100 bytes plus 4 bytes per attribute in the extended form.

The attribute count field is three bits, and **zero means eight**. Linux's
validator computes the size from those bits and requires the record reference's
low four bits to be zero, then advances by the size rounded up to 16 bytes.

A shader record names:

* the fragment shader code address, its varying count, and whether it is
  single-threaded;
* the coordinate shader and vertex shader code addresses, their attribute
  select bits and total attribute sizes;
* per attribute: address, size in bytes minus one, stride, and the VPM offsets
  for the coordinate and vertex shaders.

So the shader record is a **description of the shader's memory layout**, written
by the driver, consumed by the hardware. It is the natural place for the
compiler's metadata to become hardware state, and the reason the compiler must
report its VPM and attribute layout rather than only machine code.

### 4.6 Uniforms and textures

Uniforms are a separate stream in memory, addressed by the uniform pointer that
a QPU program writes into its own `UNIFORMS_ADDRESS` register (write address 40)
and then reads sequentially through read address 32.

Texture state lives **inside the uniform stream**. A texture descriptor is four
32-bit words at offsets that the shader record declares; Linux's validator reads
those words, decodes the texture's base offset, type, dimensions and mip levels,
computes the required size from the tiling and pixel format, and checks it
against the buffer object that the descriptor's handle names. It also checks a
uniform-buffer descriptor's offset and clamp against the buffer size, so that a
shader cannot read outside the range the application declared.

This is the single most important structural fact in this document for the
submission design: **most of the addresses the GPU uses are not in the shader,
they are in the uniform stream and the render configuration.** A texture
descriptor comes from uniforms, and a render target's address comes from the
render configuration.

But it is not the whole story, and the exception is the one that matters: a
shader *can* compute an address and reach memory through the VPM's
general-purpose DMA mode or a direct-addressed TMU lookup (§6.5). So controlling
the uniform stream is necessary but not sufficient. What completes the argument
is that the compiler emits a subset with no addressing route and the kernel
checks it. The uniform stream's role is then to make the *legitimate* addresses
driver-chosen, and the subset's role is to remove the illegitimate ones.

### 4.7 Primitive setup and limits

* Vertex data is read by the vertex shader through attribute records; each
  attribute is an address, a stride and a size. The vertex format stores signed
  12.4 fixed-point offsets from the viewport centre, so a render target larger
  than 4096 in either dimension is out of contract. The texture unit samples at
  most 2048×2048.
* Indexed primitives carry a maximum index, which the validator uses to bound
  the vertex buffer access. The index buffer access is checked as
  `offset + length * index_size <= buffer size`.
* Primitives are points, lines, triangles or right-hand triangles, with 8- or
  16-bit indices.
* Clipping is guardband: primitives rasterise outside the view volume, so the
  clip window must be set to the viewport even when the application did not ask
  for a scissor.

---

## 5. Tiling and surface layout

Two tiling formats matter, plus linear:

* **T** — 64-byte microtiles. A microtile is 4×4 pixels at 4 bytes per pixel,
  8×4 at 2, 8×8 at 1. The width is padded to a multiple of eight microtiles and
  the height to eight microtiles.
* **LT** — linear-tile, used for small surfaces: width and height padded to one
  microtile.
* **Linear** — row-major with a padded stride.

The rule Linux uses to choose is: LT when the width is at most four microtiles
**or** the height is, otherwise T; and linear only for a special texture type.

This matters to this project because the existing compositing driver's surface
is **linear XRGB32 with stride = width × 4**. A GPU render target can be linear
with that exact stride, which is what makes destination model (a) in
`docs/QUICKDRAW-HW-DESIGN.md` — write the same bytes software would have
written — reachable at all. A tiled target would not be byte-identical to the
software surface without a detile pass.

---

## 6. Memory rules

### 6.1 Addresses

The GPU is a bus master with no MMU on this part. Every address it is given is a
physical (bus) address. The ARM's CPU pointer is not that address: a buffer must
be allocated through the device service, which returns both a CPU address and a
device address, and only the device address may appear in a control list, a
shader record, a uniform, or a tile/binner pointer.

Linux's own words for this, in the validator's file comment, are the clearest
statement of the threat: without an IOMMU, a user with control-list access could
escalate privilege by drawing to system memory as if it were a framebuffer, or
read memory it should not by naming it as a vertex or index buffer.

### 6.2 Why validation is not a proof

That comment is also the reason `docs/GPU-SUBSYSTEM.md` §6 argues for
construction rather than validation. The validator has to:

* whitelist record types and check each record's length against the list length;
* track shader records, their sizes and their alignment;
* bound index-buffer access and track the maximum index per shader state;
* validate binner configuration, tile-state size and tile-alloc size;
* relocate every address in the list, the shader records and the uniform stream;
* decode every texture descriptor in the uniform stream and check its size,
  tiling and mip chain against the buffer it names;
* check uniform-buffer offsets and clamps;
* reject unsupported flags and combinations;
* and validate the shader instruction stream separately, because a shader is a
  program the hardware executes.

Each of those is a place where a missed check is a privilege escalation. The
list is not evidence that the approach is wrong for Linux — it is evidence that
"validate an arbitrary list" means "implement all of the above soundly". The
project's alternative, in `docs/GPU-SUBSYSTEM.md` §6, is to never accept a list.

### 6.3 Cache

The GPU does not snoop the ARM's caches. Before submitting work that names a
buffer, the CPU must clean the buffer to the point of coherency; after the GPU
has written a buffer and before the CPU reads it, the CPU must invalidate its
own lines. The existing mailbox driver already does exactly this for the
firmware (`docs/VIDEOCORE-2D-DESIGN.md` §4), and the same discipline applies
here. A no-op "cache maintenance" is correct only where the cache never holds
the data, which is not this hardware.

### 6.4 Alignment and placement

| Object | Requirement | Source |
|---|---|---|
| Shader record | 16-byte aligned; size rounded up to 16 | Linux validator |
| Tile state | page-aligned, zeroed before use | Linux validator |
| Binner pool | must not cross a 256 MiB boundary; 512 KiB per-job slots | Linux driver |
| Texture base | 4 KiB granularity (the descriptor's offset field is bits 31:12 of the address) | Texture descriptor layout |
| Control list | 16-byte alignment is the safe assumption; the exact requirement was not extracted | Not established — §12 |

### 6.5 What the GPU can and cannot address

It can address: control lists, shader code, shader records, uniforms, vertex and
index buffers, textures, render targets, tile state and tile alloc, and the
binner overflow region.

It cannot address: the ARM's MMIO, because the peripheral aperture is outside
the RAM the GPU fetches from — but this is a property of the bus, not a
protection, and a wrong physical address inside RAM is simply a wrong address.
There is no fault reported to the ARM for a bad GPU address; the visible symptom
is corrupted output, a hang, or an error bit.

**A QPU program can reach memory, and this must not be glossed over.** The
shader validator's own threat statement names three routes, and they are real:

1. **The VPM in general-purpose DMA mode.** The VPM configuration register has a
   mode that transfers between the VPM and *system memory*, with the address
   coming from registers the shader writes. A shader in that mode can write
   anywhere in RAM.
2. **A direct-addressed TMU lookup.** A texture read can be configured to take
   its address directly rather than through a descriptor base, with the address
   supplied in the uniform stream and an offset the shader computes. Linux's
   validator tracks per-register minimum offsets, maximum clamps and immediates
   precisely so it can prove the computed address stays inside the buffer.
3. **Uniform data reads.** A shader reads the uniform stream sequentially; if an
   attacker controlled that stream, the shader could read whatever it names.

The first two are the reason a shader is a *program* and not just arithmetic,
and the reason a submission design cannot simply assert "the shader cannot name
an address". The design response
(`docs/GPU-SUBSYSTEM.md` §6.3, `docs/SHADER-COMPILER-DESIGN.md` §4.10) is to
constrain the compiler to a **subset of the ISA with no addressing capability**,
and to have the kernel check that the subset was used:

* the VPM configuration and address registers are written only with
  compiler-emitted immediates whose mode field selects register-file-to-VPM
  transfer, never general-purpose DMA;
* texture sample metadata never declares direct addressing, and the driver
  refuses a program that does;
* the uniform stream is driver-built, so route 3 names only what the driver
  chose.

That is a structural closure rather than a dataflow proof, and §12 records what
remains unverified about it.

---

## 7. The QPU instruction set

### 7.1 Execution model

* A QPU is a **16-wide SIMD** processor: four lanes, each executing four
  cycles' worth of work, so one instruction operates on sixteen elements.
* It is **dual-issue**: each 64-bit instruction carries one add-ALU operation
  and one mul-ALU operation, which execute in the same cycle. Either may be a
  no-op.
* Each QPU has a **register file of 32 A registers and 32 B registers**, plus
  accumulators `r0`–`r3`, `r4` (pack/unpack) and `r5` (small immediate or
  rotate). Registers are 32 bits wide per element.
* There is **no call stack, no integer division, and no general memory
  load/store instruction.** Memory is reached through the VPM's DMA modes
  (including the general-purpose mode that reaches system memory — §6.5), the
  texture unit, and the tile buffer.
* Branches are **whole-QPU**: all sixteen elements take the branch. There is no
  per-lane divergence.
* A program ends with a signal that tells the scheduler the thread is done; the
  **two instructions following a branch or a program end are delay slots and
  always execute**.

### 7.2 Instruction encoding

64 bits, little-endian. Field positions:

| Bits | Field | Meaning |
|---|---|---|
| 63:60 | signal | What the instruction signals (branch, program end, thread switch, load immediate, TMU load, …). |
| 59:57 | unpack | Unpack mode applied to the A-file read, or to `r4`. |
| 56 | `pm` | Pack mode applies to the mul ALU / `r4` instead of the A file. |
| 55:52 | pack | Pack mode applied to the A-file write. |
| 51:49 | `cond_add` | Condition under which the add ALU writes. |
| 48:46 | `cond_mul` | Condition under which the mul ALU writes. |
| 45 | `sf` | Set flags from the add ALU result. |
| 44 | `ws` | Swap which file is written by the add and mul ALUs. |
| 43:38 | `waddr_add` | Write address for the add ALU. |
| 37:32 | `waddr_mul` | Write address for the mul ALU. |
| 31:29 | `op_mul` | Mul ALU opcode. |
| 28:24 | `op_add` | Add ALU opcode. |
| 23:18 | `raddr_a` | A-file read address. |
| 17:12 | `raddr_b` | B-file read address; also the small-immediate field when the signal is "small immediate". |
| 11:9 | `add_a` | Add ALU first operand mux. |
| 8:6 | `add_b` | Add ALU second operand mux. |
| 5:3 | `mul_a` | Mul ALU first operand mux. |
| 2:0 | `mul_b` | Mul ALU second operand mux. |

### 7.3 Signals

| Value | Signal | Effect |
|---|---|---|
| 0 | breakpoint | Software breakpoint. |
| 1 | none | Ordinary instruction. |
| 2 | thread switch | Yield to the other thread on this QPU. |
| 3 | program end | Thread finished; two delay slots follow. |
| 4 | wait for scoreboard | Stall until outstanding writes complete. |
| 5 | unlock scoreboard | Release a scoreboard lock. |
| 6 | last thread switch | Yield, and signal that this was the last thread. |
| 7 | coverage load | Load coverage from the tile buffer. |
| 8 | colour load | Load colour from the tile buffer into `r4`. |
| 9 | colour load and thread end | As above, then end. |
| 10 | load TMU0 | Move TMU0 result into the register file. |
| 11 | load TMU1 | Move TMU1 result into the register file. |
| 12 | alpha mask load | Load the alpha mask. |
| 13 | small immediate | The B read address field is a small immediate constant. |
| 14 | load immediate | The low 32 bits are an immediate; the mode field selects 32-bit, 2×16-bit or 2×8-bit packing. |
| 15 | branch | The instruction is a branch (§7.6). |

Signal 14 with mode 4 is **not** a load immediate but a **semaphore
instruction**, carrying a 4-bit semaphore number and an acquire/release bit.
That is how QPU threads synchronise with each other and with the control-list
processor without the fixed-function pipeline's semaphores.

### 7.4 Operand muxes and registers

| Mux | Source |
|---|---|
| 0–3 | accumulators `r0`–`r3` |
| 4 | `r4` |
| 5 | `r5` |
| 6 | A file, at `raddr_a` |
| 7 | B file, at `raddr_b` |

Read addresses:

| Address | Read location |
|---|---|
| 0–31 | A or B register file |
| 32 | uniforms (sequential; broadcast to all lanes) |
| 35 | varying |
| 38 | element number (A) / QPU number (B) |
| 39 | null |
| 41 | X pixel coordinate (A) / Y pixel coordinate (B) |
| 42 | multisample reverse flags (A) / reverse flag (B) |
| 48 | VPM read |
| 49 | VPM load busy (A) / VPM store busy (B) |
| 50 | VPM load wait (A) / VPM store wait (B) |
| 51 | mutex acquire |

Write addresses:

| Address | Write location |
|---|---|
| 0–31 | A or B register file |
| 32–35 | accumulators `r0`–`r3` |
| 36 | TMU no-swap |
| 37 | accumulator `r5` |
| 38 | host interrupt |
| 39 | null |
| 40 | uniforms address (sets the uniform stream pointer) |
| 41 | quad X (A) / quad Y (B) |
| 42 | multisample flags (A) / reverse flag (B) |
| 43 | tile-buffer stencil setup |
| 44 | tile-buffer Z |
| 45 | tile-buffer colour, multisampled |
| 46 | tile-buffer colour, all samples |
| 47 | tile-buffer alpha mask |
| 48 | VPM write |
| 49 | VPM VCD setup (load in A, store in B) |
| 50 | VPM address (load in A, store in B) |
| 51 | mutex release |
| 52–55 | SFU: reciprocal, reciprocal square root, exp2, log2 |
| 56–59 | TMU0: S, T, R, B |
| 60–63 | TMU1: S, T, R, B |

### 7.5 ALU operations

Add ALU: `nop`, `fadd`, `fsub`, `fmin`, `fmax`, `fminabs`, `fmaxabs`, `ftoi`,
`itof`, `iadd`, `isub`, `shr`, `asr`, `ror`, `shl`, `imin`, `imax`, `band`,
`bor`, `bxor`, `bnot`, `clz`, `v8adds`, `v8subs`.

Mul ALU: `nop`, `fmul`, `imul24`, `v8muld`, `v8min`, `v8max`, `v8adds`,
`v8subs`.

Two things follow for a compiler:

* **Integer add, subtract, shifts, min/max, bitwise, count-leading-zeros are
  all present.** Integer arithmetic is exact, and a QuickDraw operation can be
  expressed without floating point at all.
* **There is no integer multiply.** The only multiply is `fmul` (float) and
  `imul24` (a 24-bit integer multiply). A 32-bit integer multiply must be built
  from `imul24` plus correction, or avoided. This is a real constraint on the
  shader language's integer semantics and is recorded as such.

The `v8*` operations treat a 32-bit register as four 8-bit lanes, which is how
four 8-bit pixels are processed per register — the basis of a fast blit.

### 7.6 Branches

A branch instruction reuses the 64-bit word:

| Bits | Field |
|---|---|
| 55:52 | branch condition (12 conditions: all/any × zero-set/zero-clear, negative-set/clear, carry-set/clear, plus "always") |
| 51 | relative |
| 50 | register-relative (target taken from `raddr_a`) |
| 49:45 | `raddr_a` when register-relative |
| 44 | `ws` |
| 43:38 / 37:32 | add/mul write addresses, applied in the delay slots |
| 31:0 | target (absolute instruction index, or a signed offset when relative) |

The two instructions after the branch execute unconditionally. A compiler must
fill them, and the instructions it puts there are subject to the restrictions in
§9.4.

### 7.7 Small immediates

A small immediate is a 6-bit field:

* 0…15 → the integers 0…15;
* 16…31 → −16…−1;
* 32…39 → 1.0, 2.0, … 128.0;
* 40…47 → 1/256, 1/128, … 1/2;
* 48 → rotate by `r5`;
* 49…63 → rotate by *n* channels.

The float encodings are powers of two only. Any other constant needs a load
immediate instruction.

### 7.8 Pack and unpack

Pack and unpack convert between 32-bit registers and 8- or 16-bit sub-word
values, and are how a QPU processes four 8-bit pixels or two 16-bit values per
instruction. Unpack applies to the A-file read (or to `r4`); pack applies to the
A-file write.

Unpack modes: none, 16a, 16b, repeat-8d, 8a, 8b, 8c, 8d. Pack modes: none, 16a,
16b, replicate-8, 8a, 8b, 8c, 8d, and saturating variants of each.

The guide's Table 8 is mislabelled and its 8b/8c/8d rows are wrong; the
encodings above are the ones implementations emit. This is recorded in §9.4.

### 7.9 The SFU

Four special functions, written by address: reciprocal, reciprocal square root,
`exp2`, `log2`. Semantics: log and exp are base 2; `recip(0)` is infinity;
`recipsqrt(x)` is `1/sqrt(|x|)`; `log(x)` is `log(|x|)`. Results arrive in `r4`,
which imposes the scheduling restrictions in §9.4.

### 7.10 The VPM

The VPM is an on-chip memory shared by the fixed-function pipeline and the QPUs.
A QPU reads it (read address 48) and writes it (write address 48). Transfers
between the VPM and the register file are set up by writing the VPM VCD setup
register (write address 49) and the VPM address register (write address 50), and
flow control is observed by reading the load/store busy and wait addresses (49
and 50).

Its total size is reported by `V3D_IDENT1` in KiB, and `VPMBASE` partitions it
between the fixed-function pipeline and standalone programs. Because a vertex
shader's outputs and a fragment shader's varyings both live in the VPM, the
**compiler must allocate VPM offsets and report them**, and the driver must put
those offsets in the shader record and in the varying/attribute state.

### 7.11 The TMU

Two texture units per QPU are addressable: TMU0 (write addresses 56–59 for S, T,
R, B) and TMU1 (60–63). A texture fetch writes the coordinate to S/T (and the
level of detail to R or B), then the result is moved into the register file by
the `load TMU0`/`load TMU1` signal, which writes `r4` and then the destination.

`TMU_NOSWAP` (write address 36) is a TMU configuration write with a required
three-instruction separation from the first TMU write that depends on it. Its
exact effect was not extracted; what the backend must do is observe the
separation rule.

The texture descriptor is in the uniform stream (§4.6), so the TMU's addresses
are never in the shader.

---

## 8. Queues, semaphores and completion

### 8.1 The two list processors

Binning and rendering are separate list processors. Each has a current address
(`CTnCA`), an end address (`CTnEA`), a status word (`CTnCS`), a loop count
(`CTnLC`) and a program counter (`CTnPC`). Writing the current address starts
execution from that address; the end address bounds it. The status word reports
running, sub-list active, error and semaphore activity; the remaining named bits
(`CTRSTA`, `CTRTSD`, `CTMODE`) were not interpreted in this pass.

Completion of the render list sets `INTCTL.FRDONE`; completion of binning sets
`FLDONE`.

### 8.2 Semaphores

The block has a number of semaphores, reported by `V3D_IDENT1` (sixteen is the
expected count for this class). They can be incremented and waited on by
control-list records, and acquired and released by QPU instructions. The
conventional use is a handshake: binning ends with an increment, rendering waits
on the semaphore, so the render list cannot consume a bin list before it is
complete.

Semaphores are also the natural hardware primitive for **fences** between
submissions. They are a global, finite resource, so a design that uses them for
per-context fences must allocate them from a pool and must not let a context
exhaust them.

### 8.3 The QPU scheduler queue

Standalone QPU programs (not part of the fixed-function pipeline) are dispatched
through the scheduler queue: `SRQPC` takes a program address, `SRQUA`/`SRQUL`
take the uniform stream address and length, and `SRQCS` reports queue status.
This is the path the firmware uses for GPGPU, and it is the path this project
would use if it ever exposed raw compute. It is **not** used by the 3D pipeline,
which dispatches shaders through the shader record instead.

### 8.4 What completion means

A render-list completion interrupt means the list processor has finished walking
the list and the pipeline has drained. It does **not** mean the CPU's view of the
render target is coherent: the CPU must invalidate before reading. It also does
not mean a later list has not started; the driver must not overwrite a list that
the processor may still be walking, which is why the current-address register is
read back as the acknowledgement of a submission.

---

## 9. Instruction restrictions

These are hardware rules, not compiler preferences. The list below is the guide's
page-37 summary as restated by Mesa's validator, plus the errata. A backend that
violates one produces a program that fails unpredictably on silicon and passes
every host test that does not model the rule.

### 9.1 Program end

* The thread-end instruction must not write to a physical A or B register.
* It must not trigger an implicit scoreboard wait (no tile-buffer access).
* The **two delay slots after it execute**, so a program is at least three
  instructions long in effect.

### 9.2 The last three instructions

* None of the thread-end instruction or its two delay slots may read a varying,
  read a uniform, or read or write the VPM.
* None of them may read or write A/B register 14.
* The final instruction must not write the tile-buffer Z value.

### 9.3 The first two instructions of a fragment shader

* No scoreboard wait, explicit or implicit (no tile-buffer access).

### 9.4 Other restrictions

* **No read of a register written by the immediately preceding instruction.**
  The pipeline has no forwarding; the compiler must insert a move or a
  non-conflicting instruction.
* **`TMU_NOSWAP` must be written at least three instructions before the first
  TMU write that depends on it.**
* **After an SFU write, `r4` must not be read for two instructions**, and no
  other instruction that writes `r4` (TMU load, tile-buffer read, another SFU
  write) may occur in those two instructions.
* **The full horizontal vector rotate is only available when both mul-ALU inputs
  are accumulators `r0`–`r3`.**
* **A rotate by `r5` must not immediately follow a write to `r5`.**
* **A vector rotate must not immediately follow a write to an accumulator** (the
  guide's restriction, as checked by the validator).
* The guide's Table 8 is the R4 **unpack** table, and its 8b/8c/8d rows are
  wrong. The encodings in §7.8 are the correct ones.
* A QPU program is limited in length by the instruction memory; the exact limit
  was not extracted here and is a backend admission check (§12).

### 9.5 Consequences for a compiler

1. A **hazard-aware scheduler** is mandatory. Emitting instructions in
   source order will violate §9.4 on almost any real program.
2. **Delay slots must be filled**, and the filler is subject to §9.2.
3. **No divergence** means data-dependent `if` must become predication or
   `select`; a data-dependent loop is not expressible.
4. **No spilling** means register pressure is an admission criterion, not a
   performance heuristic.
5. **No integer multiply** means 32-bit integer products must be synthesised or
   avoided.
6. **The emitted subset must exclude the addressing routes.** The VPM's
   general-purpose DMA mode and direct-addressed TMU lookups are how a shader
   reaches memory (§6.5). A backend that emits them, or that computes the VPM
   configuration at run time, hands an untrusted program the ability to name an
   address. The subset rule is in `docs/SHADER-COMPILER-DESIGN.md` §4.10 and the
   kernel checks it.

---

## 10. What the second target needs

Not part of the first landing, recorded so the compiler design does not have to
be re-argued.

| | VC4 (V3D 2.1) | V3D 4.x | V3D 7.x |
|---|---|---|---|
| QPU ISA | 64-bit, dual-issue add/mul, 16-wide | different encoding and register model | different again |
| Address translation | none — physical addresses | MMU with GPU virtual addresses | MMU |
| Control lists | the records in §4 | a different, per-generation record set | different again |
| Public documentation | full architecture guide | V3D 4.1 guide | not publicly documented in the same form; Mesa is the reference |

The consequences are: the *submission* design's no-IOMMU argument is specific to
VC4 and must be restated (and can be relaxed) for a target with an MMU; and the
*compiler* front end and IR are unaffected, which is the point of
`docs/SHADER-COMPILER-DESIGN.md`.

Mesa's V3D driver carries per-generation control-list descriptions in the shared
Broadcom directory, and the kernel `v3d` driver has a separate MMU module — both
are evidence that the second target is a new backend and a new submission
profile, not a variant of the first.

---

## 11. Sources

**Primary documentation**

* *VideoCore IV 3D Architecture Reference Guide*, `VideoCoreIV-AG100-R`,
  Broadcom, 16 September 2013: §3 (QPU register map and instruction set),
  §4–§8 (fixed-function pipeline, tile buffer, texture unit), §9 (control lists,
  binner, tile memory), Appendix B Table 95 (V3D base address), errata table,
  page 37 ("Summary of Instruction Restrictions").

**Implementations, cited for facts**

* Mesa, `src/gallium/drivers/vc4/`: `kernel/vc4_packet.h` (MIT);
  `vc4_draw.c`, `vc4_emit.c`, `vc4_job.c`, `vc4_context.c` (MIT);
  `vc4_qpu_defines.h`, `vc4_qpu_validate.c`, `vc4_register_allocate.c` (MIT).
* Linux, `drivers/gpu/drm/vc4/`: `vc4_validate.c` (MIT),
  `vc4_validate_shaders.c` (MIT), `vc4_v3d.c` (GPL-2.0-only),
  `vc4_regs.h` (GPL-2.0-only).
* Linux, `arch/arm/boot/dts/broadcom/bcm2835-common.dtsi` (GPL-2.0):
  `v3d@7ec00000`, the PM node's clock list, the DMA channel mask.
* Linux, `Documentation/devicetree/bindings/interrupt-controller/brcm,bcm2835-armctrl-ic.yaml`
  (kernel binding documentation): the bank/bit interrupt encoding and the named
  source list that makes `<1 10>` the 3D block's interrupt.
* QEMU, `include/hw/arm/raspi_platform.h` and `hw/arm/bcm2835_peripherals.c`
  (GPL-2.0): `V3D_OFFSET 0xc00000`, `INTERRUPT_3D 10`, the peripheral base and
  the unimplemented-device registration. Cited for facts about the model, not
  about the silicon.
* `py-videocore` (Idein), MIT: `README.rst`, `APPENDIX.rst`,
  `videocore/encoding.py`, `videocore/driver.py`.

**Second target**

* Mesa, `src/gallium/drivers/v3d/` and `src/broadcom/` (MIT).
* Linux, `drivers/gpu/drm/v3d/` (GPL-2.0-only), including its MMU module and
  GPU-VM buffer allocation.

**This repository**

* `docs/VIDEOCORE-2D-DESIGN.md` §2 (address spaces), §7 (the V3D and the QPU),
  §10 (what QEMU models), §15 (residual uncertainties).
* the archived BCM2837 hardware inventory — its V3D row is **partly wrong**: the address
  is correct, and the size, interrupt count, category and reachability claim are
  not. See §2.7 for the derivation and the evidence. Every other row of that
  inventory used here (DMA channel mask, clock manager, PM block) agrees with
  the device tree.
* `docs/TIMED-OUTPUT-CLASS.md` — the job model that `docs/GPU-SUBSYSTEM.md` §5.4
  adopts in shape and not in feeder.

---

## 12. What is not established

Each item names what would resolve it.

1. **`SQCNTL`/`SRQCS` field layout.** The queue registers are identified and
   their purpose is clear, but the bit fields were not extracted. Resolution:
   guide §3, then a board read.
2. **Exact QPU instruction-memory size.** The program-length limit is a backend
   admission bound. Resolution: guide, then a board run that loads a program of
   known length.
3. **Control-list alignment.** 16-byte alignment is assumed from the shader
   record rule; the control list's own requirement was not found. Resolution:
   guide §9; until then allocate 16-byte aligned lists.
4. **VPM total size on this part.** Reported by `V3D_IDENT1[31:28]`; the value
   for this board was not read. Resolution: one register read at bring-up.
5. **Slice/QPU/TMU counts.** Derived from `V3D_IDENT1`; "twelve QPUs" is an
   expectation, not a measurement.
6. **Semaphore count.** `V3D_IDENT1[23:16]`; sixteen expected, not measured.
7. **`VPMBASE` partitioning.** The register is understood; the effect of a
   non-zero partition on the fixed-function pipeline's VPM budget is not
   quantified. Resolution: a board experiment that renders with several
   partitions.
8. **Whether the V3D is enabled by the firmware on this boot path.** Linux
   enables the clock and releases the reset itself; `py-videocore` asks the
   firmware to enable the QPU. Whether a bare-metal ARM can always reach the
   block without a firmware step is the bring-up question that `V3D_IDENT0`
   answers. Resolution: read `V3D_IDENT0` after clock/reset on the board.
9. **QEMU.** `docs/VIDEOCORE-2D-DESIGN.md` §10 measured the V3D as an
   unimplemented placeholder returning zero. Its address is the silicon's
   (`0x3FC00000`, §2.7); the earlier claim that the model's address differed was
   a transposition in that document, not a property of the model. The model
   still has no binner, QPU, tile buffer or interrupt, so §3.5's split stands.
10. **Timing.** No claim is made here about shader throughput, TMU latency, VPM
    bank conflicts or binner throughput. All are board measurements.
11. **The VPM configuration register's mode encoding.** The existence of a
    general-purpose-DMA mode (VPM ↔ system memory) and a register-file-transfer
    mode is established by the shader validator's threat statement and by the
    register names; the bit positions that distinguish them were not extracted.
    Resolution: guide §3, then a board run. Until then, the compiler emits the
    configuration value as an immediate and the checker verifies it against the
    value the driver independently computes — two independent derivations, not
    one.
12. **Whether the subset check is complete.** The design closes the VPM-DMA and
    direct-TMU routes structurally. Whether there is a third route — an
    instruction sequence that reaches memory without using either — is not
    established, and the answer decides whether the residual risk in
    `docs/GPU-SUBSYSTEM.md` §6.4 is acceptable. Resolution: an adversarial
    program per escape route, on the board.

---

## 13. Board runs this research needs

Each line is a check with an observation, not a wish. They are ordered so that a
failure early stops work that depends on it.

1. **The block is reachable.** Clock enabled, reset released, `V3D_IDENT0` reads
   `0x02443356`. If it does not, the address or the clock/reset sequence is
   wrong and nothing else in this document can be tested.
2. **The counts are real.** `V3D_IDENT1` gives slice, QPU, TMU, semaphore and
   VPM counts. Compare with the "twelve QPUs" expectation; a mismatch means the
   part is not the one assumed.
3. **One list runs.** A bin list that only clears, and a render list that only
   stores a tile buffer, complete with `FLDONE` and `FRDONE`.
4. **One triangle.** A shader record, an attribute, a primitive, a readback, and
   the expected bytes.
5. **One texture.** A descriptor in the uniform stream, a fetch, and the
   expected colour.
6. **The restrictions are real.** One program per rule in §9, each expected to
   be refused by the checker and — separately — to misbehave if run anyway.
7. **The subset is closed.** One program per escape route (§6.5): a computed
   VPM configuration write, a direct-address sample declaration. Both must be
   refused, and the test must be run on silicon because the checker's
   completeness is exactly what is in question.
8. **The binner overflow path.** Geometry that exceeds the initial tile alloc,
   to confirm the overflow region works and that `OUTOMEM` is reported when it
   does not.
9. **A reset.** After a deliberate hang, the block comes back and a known job
   produces the known bytes.
10. **Timing.** Shader throughput, TMU latency and the cost of a job, so that
    the compiler's scheduling gaps and the subsystem's timeout have measured
    bases.

---

## 14. Decisions a reviewer must make

The consolidated list is §16.1 — decisions D1–D29, each naming the section that
argues it. The four that originate in this document are:

| Id | Decision | Argued in |
|---|---|---|
| **D10** | The guide is the authority and the encoders are the tie-break where they disagree (pack/unpack). | §2.1, §9.4 |
| **D3** | `VPMBASE = 0` for the first landing. | §3.3 |
| **D4** | The first render target is linear XRGB32. | §5 |
| **D14** | The semaphore budget for fences. | §8.2 |

D1/D2 (construction, and the no-addressing subset) also rest on this document's
§6.2, and the record corrections this document found are §16.3. The lists are
not repeated here: a duplicated decision is a decision that can drift from its
own argument, which is the defect this document was corrected for.

---

## 15. Hardware fact register

One table of every hardware fact the five documents rely on, its provenance, and
where it appears. It exists because a single transposed digit in one document
propagated into three and survived until an outside check caught it; a value
that appears in more than one place is a value that can disagree with itself.

**Provenance codes.** `G` — Architecture Reference Guide. `M` — Mesa `vc4`
(MIT). `L` — Linux `drm/vc4` (GPL-2.0, facts only). `D` — device tree or binding
(GPL-2.0). `P` — `py-videocore` (MIT). `Q` — QEMU model (GPL-2.0; a fact about
the model, not the silicon). `I` — this project's hardware inventory (a weaker
source: it is derived, and its V3D row was wrong in four fields). `X` — derived
by this document from other facts, with the derivation shown.

| Fact | Value | Provenance | Appears in |
|---|---|---|---|
| V3D bus base | `0x7EC00000` | G, D | QPU §2.7, §3.1; QD-HW §2 (diagram) |
| V3D ARM base | `0x3FC00000` | X from D | QPU §2.7, §3.1; QD-HW §2 |
| Peripheral bus base | `0x7E000000` | G §1.2.4 | QPU §2.7 |
| ARM peripheral aperture | `0x3F000000`, 16 MiB | I | QPU §3.1 |
| Bus↔ARM rule | `ARM = bus − 0x7E000000 + 0x3F000000` | X, calibrated on RNG and HVS | QPU §2.7 |
| V3D window size | `0x1000` | D | QPU §2.7, §3.1 |
| V3D ARM interrupt | GPU interrupt 10 (bank 1 bit 10 = `VC_3D`) | D, L binding, Q | QPU §2.7, §3.1; GPU §1.1, §3 |
| V3D internal status bits | `FRDONE` 0, `FLDONE` 1, `OUTOMEM` 2, `SPILLUSE` 3 | L | QPU §3.4; GPU §5.3 |
| `V3D_IDENT0` expected | `0x02443356` | L | QPU §3.2; GPU §3; QD-HW §11 |
| `V3D_IDENT1` fields | VPM 31:28, semaphores 23:16, TMUs 15:12, QPUs 11:8, slices 7:4, rev 3:0 | L | QPU §3.2 |
| Register offsets `0x00000`–`0x00F20` | see §3.3 | L, G | QPU §3.3 |
| `L2CACTL` bits | clear 2, disable 1, enable 0 | L | QPU §3.3 |
| `PCS` bits | binner OOM 8, render busy 3, render active 2, binner busy 1, binner active 0 | L | QPU §3.3 |
| Tile buffer | 64×64×32bpp = 16 KiB; 32×32 at 4× MSAA | M, G | QPU §4.3, §4.4 |
| Tile state | 48 bytes/tile | L, M | QPU §4.4 |
| Tile alloc | 32 bytes/tile initially | L, M | QPU §4.4 |
| Binner pool | 16 MiB, 512 KiB slots, must not cross 256 MiB | L | QPU §4.4; GPU §10 |
| Max tiles sized for | 4096 (2048×2048 at 32×32) | X from L | QPU §4.4 |
| Shader record size | 36 + 8·attrs, or 100 + 4·attrs extended; 16-byte aligned | L | QPU §4.5 |
| Shader record attribute count | 3 bits, 0 means 8 | L | QPU §4.5 |
| Render target limit | 4096 per axis | L | QPU §4.7; QD-3D §4.1 |
| Texture limit | 2048×2048 | L | QPU §4.7; QD-3D §4.1 |
| Texture base granularity | 4 KiB (offset field is bits 31:12) | L | QPU §6.4 |
| QPU instruction | 64-bit, field positions in §7.2 | G, M, P | QPU §7.2 |
| QPU width | 16 elements, four lanes × four cycles | G, P | QPU §7.1 |
| QPU register file | 32 A + 32 B + `r0`–`r5` | M | QPU §7.1, §7.4 |
| QPU A/B registers when threaded | bottom 16 only | M | QPU §2.2; SC §4.6 |
| QPU reserved register | 14 (read-address fixup) | M | QPU §2.2; SC §4.6 |
| QPU instruction-memory size | **not established** | — | QPU §12 |
| VPM size | from `V3D_IDENT1`, **not measured** | L | QPU §7.10, §12 |
| Semaphore count | from `V3D_IDENT1`, **not measured** | L | QPU §8.2, §12 |
| QPU counts | slices × QPUs/slice; "twelve" is an expectation | P, L | QPU §1.2, §3.2 |
| `VPMBASE` first value | 0 | L | QPU §3.3; GPU §10 |
| DMA channel mask | `0x7f35` (channels 0,2,4,5,8–14) | D | QPU §2.5; GPU §5.4 |
| RNG bus / ARM | `0x7E104000` / `0x3F104000` | D, I | QPU §2.7 |
| HVS bus / ARM | `0x7E400000` / `0x3F400000` | D, I | QPU §2.7 |
| QEMU `V3D_OFFSET` | `0xc00000` → `0x3FC00000` | Q | QPU §2.7, §3.5 |
| QEMU `INTERRUPT_3D` | 10 | Q | QPU §2.7 |
| XRGB32 stride | width × 4 | this project's `VIDO` contract | QD-HW §7, §9; QPU §5 |

Two entries in that table are deliberately marked **not established** rather than
given a plausible number. A plausible number is what propagated last time.

### 15.1 Facts that appear in only one document

Not every fact needs to be repeated, but a fact that *only* one document states
is a fact no other document can be checked against. The ones that matter:

| Fact | Only in | Why it is not cross-checked |
|---|---|---|
| The resolved-span primitive's fields | QD-HW §5.2 | It is this design's invention, not a hardware fact. |
| The batch window derivation | QD-HW §6.2 | Derived from the frame interval; a policy constant. |
| The shader ABI's three parts | QD-3D §2.2 | A contract between two components of this design. |
| The crate tree | SC §8 | This design's structure. |
| The clock/reset capability's operations | GPU §1.3 | This design's interface. |

Those are design decisions, not hardware facts, and they are not expected to
appear twice. Everything in the register above is a hardware fact and does.

---

## 16. Consolidated decisions and interface gaps

The five documents each end with their own list. This is the single list, so a
reviewer has one place to work from. Identifiers are stable and are referenced
from the individual documents.

### 16.1 Decisions

| Id | Decision | Where argued | Blocks |
|---|---|---|---|
| **D1** | The kernel **constructs** control lists from checked parameters rather than validating arbitrary ones. Consequences: no application-authored lists, no raw QPU compute from user space, and the residual risk is a subset-check hole rather than a template bug. | GPU §6; QD-3D §3.1–3.2 | Everything. If rejected, the design must implement the whole of QPU §6.2. |
| **D2** | The compiler emits an **ISA subset with no addressing capability**: VPM configuration writes are immediates selecting register-file transfer, and no metadata declares direct TMU addressing. The kernel checks the subset. | SC §4.10; QD-3D §3.1 | D1's soundness. |
| **D3** | `VPMBASE = 0` for the first landing — whole VPM to the pipeline, no standalone QPU programs. | QPU §3.3; GPU §10 | Whether raw compute is a first-landing feature. |
| **D4** | The first render target is **linear XRGB32, stride = width × 4**, matching the compositing driver's surface. | QPU §5; QD-HW §7.1 | Byte-exactness. A tiled target needs a detile pass. |
| **D5** | A reset **kills every context with outstanding work**, not only the offending one. | GPU §8.2; QD-3D §7.4 | Failure semantics. |
| **D6** | **Quarantine, not free**, when completion cannot be established; quarantined memory stays charged to the device. | GPU §8.3 | Decision 5 of the brief. |
| **D7** | Register exhaustion is a **refusal**, not a spill. | SC §4.6 | Which shaders are expressible. |
| **D8** | A **purpose-built shader language** over a target-neutral IR, not a GLSL subset and not SPIR-V-only. | SC §2.2 | Who writes shaders; how a second front end plugs in. |
| **D9** | One job at a time at the block, software fences, hardware semaphores only inside a job. | GPU §7 | Scheduling; failure analysis. |
| **D10** | The guide is the authority and the **encoders are the tie-break** where they disagree (pack/unpack). | QPU §2.1, §9.4 | The correctness of the encoder. |
| **D11** | The timeout value (provisional 250 ms) and the scheduling quantum. | GPU §10.2 | Hang detection; whether a slow shader is indistinguishable from a wedged one. |
| **D12** | Whether precompiled shaders ship in the image (making determinism load-bearing) or are compiled at start-up (making compile time a boot cost). | SC §8.1, §11 | Boot time; reproducibility. |
| **D13** | Whether a privileged compute tier is in scope, so the interface is not accidentally closed to it. | QD-3D §3.2, §10 | Future GPGPU. |
| **D14** | The semaphore budget for fences, and the behaviour when exhausted. | QPU §14 | Multi-context fencing. |
| **D15** | The **first-landing divert set**: rect fills, `srcCopy` copies and scrolls, declared pattern fills, and text runs; everything else delegates. Ovals, arcs, polygons and complex regions are not accelerated. | QD-HW §4 | What is accelerated at all. |
| **D16** | The **interposer is resident and in-process**, and the hardware provider is a client of the GPU subsystem rather than a direct driver caller. | QD-HW §2 | The whole split. |
| **D17** | `Rasterise` is a **capability with a software default**, so a board with no working GPU path behaves exactly as today. | QD-HW §1, §4.3 | Degradation. |
| **D18** | The **resolved-span interface** is the boundary, and the provider may know nothing about QuickDraw. | QD-HW §5 | Whether the interface is really stateless. |
| **D19** | The **batch window derivation** (`frame_interval/16`, clamped 500 µs–2 ms), subject to tuning. | QD-HW §6.2 | Batching behaviour. |
| **D20** | The **ignored byte** policy for XRGB32 (constant or preserved), decided before the corpus is written. | QD-HW §8.3, §10 Q1 | What byte-exactness asserts. |
| **D21** | **Model (b) is not scheduled**; QD-HW §7.3 is the list of conditions, not a plan. | QD-HW §7 | Scope. |
| **D22** | The **API surface** in QD-3D §1.3, including what is deliberately absent: no mips, no multiple render targets, no instancing, no compute. | QD-3D §1.3 | Feature scope. |
| **D23** | **Blending is a shader variant**, so pipeline state is part of the program key and a mismatch is a refusal. | QD-3D §1.4 | How blending is expressed. |
| **D24** | The driver's **template is a closed set of list shapes**; a job needing another shape is refused, not hand-built. | QD-3D §5.3 | D1's tractability. |
| **D25** | The **no-divergence rule**: a non-uniform `if` is a compile error unless it can be selected. | SC §2.3, §4.1 | Which shaders compile. |
| **D26** | The **checker is production code** and runs on every compile, in the shipping configuration. | SC §5.1 | D2's enforcement. |
| **D27** | The **development-tool form** of the interpreter, golden corpus and differential generator — not a revived `#[test]` framework, per the recovery order. | SC §5.2–5.6 | Whether validation work may proceed now. |
| **D28** | The **crate split**, in particular `mac-shader-qpu-isa` as the single ISA definition shared by the compiler and the kernel. | SC §8 | One checker or two. |
| **D29** | The **driver/subsystem split** in GPU §1, including the `GPUX` class and the clock/reset capability. | GPU §1 | Where each half lives. |

### 16.2 Interface gaps to route

| Id | Gap | Where | Blocking? |
|---|---|---|---|
| **G1** | The video driver **receives a device address from the Device service and drops it**; the surface handover carries only a CPU address. | QD-HW §9.2 | **Yes** — the GPU cannot name the surface. |
| **G2** | Cache maintenance is private to the video driver; there is no handover operation. The resident DMA module's buffers are coherent by contract, so the mechanism exists. | QD-HW §9.2 | **Yes** for correctness unless the drawable is coherent. |
| **G3** | No **device-side in-flight hold** on the video driver's surface; `RELEASE` drains CPU views only. | QD-HW §9.2 | **Yes** — decision 5 of the brief. |
| **G4** | `SERVICE(damage)` carries no producer identity, so a present cannot order itself after GPU completion. | QD-HW §9.2 | No — "wait before present" is an acceptable interim answer. |
| **G5** | Damage is a single rectangle, so a multi-region batch presents a bounding box. | QD-HW §9.2 | No — correct but wasteful. |
| **G6** | A mode change replaces the surface with no notification to another device. | QD-HW §9.2 | No if G3 is fixed. |
| **G7** | The **QuickDraw destination is a caller-supplied pixel buffer** with no allocation identity, so the interposer cannot assume it is the scanout surface. | QD-HW §9.2 | **Yes** — it decides which surface to render into. |
| **G8** | The `GPUX` class identifier is new and must be **registered centrally**; the driver's device match key must be confirmed against the record rather than assumed. | GPU §1.1 | No — but a wrong key means the driver never attaches. |

### 16.3 Record corrections to route

| Record | Correction | Evidence |
|---|---|---|
| the archived BCM2837 hardware inventory, V3D row | Size 64 KiB → `0x1000`; interrupts "66, 67"/"three lines" → one line, GPU IRQ 10; category VideoCore → ARM; "no supported way to submit jobs" → false | QPU §2.7 |
| `docs/VIDEOCORE-2D-DESIGN.md` §7, §10 | ARM address `0x3FEC0000` → `0x3FC00000`; the claim that QEMU's V3D address differs from the silicon's is withdrawn | QPU §2.7 |
| `platforms/raspi3b.json` (platform lane) | The V3D window must carry ARM `0x3FC00000`, not `0x3FEC0000`, if the ARM address is what the record stores | QPU §2.7 |



