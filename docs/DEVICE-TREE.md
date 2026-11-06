# Device tree

The device tree is the contract between the bootloader and the kernel. It is a
protocol, on the same footing as `platforms/*.json` for the build system and
`module.json` for module contracts: it is declared, not inferred, and both
consumers read the same blob.

## Flow

```
machine supplies  →  bootloader validates  →  kernel walks
   (at boot)          (header, size, place)      (one consumer)
```

**The tree comes from the machine.** The bootloader takes the pointer the
machine hands it, checks the header, and passes that one blob to the kernel. It
does not choose between sources and does not merge them: a platform has exactly
one tree, and if two ever existed the kernel would have to pick, which is the
ambiguity this forbids.

Nothing downstream hardcodes a peripheral address the tree declares, so a device
that moves is a change in the machine's description and nothing else.

## Where each machine's tree comes from

| machine | source | mechanism |
|---|---|---|
| `-machine virt` (`qemu_arm64`) | QEMU's own generated tree | pointer in `x0` |
| RISC-V firmware (`qemu_riscv`) | the firmware's tree | pointer in `a1` |
| BCM2837-class board | `start.elf` loads the board DTB | pointer in `x0` |
| `-machine raspi3b` | **generated**, because that machine has none | `-dtb`, returned in `x0` |

The last row is the only case where anything is generated, and the reason is
measured rather than assumed: `-machine raspi3b,dumpdtb=…` answers
*"This machine doesn't have an FDT"*, and the bootrom reports `x0 = 0` there
while reporting `x0 = 0x48000000` under `-machine virt`. QEMU states the
mechanism in that same message: supply a tree with `-dtb`, which that machine
accepts.

## The generation flag

`emulation.generate_device_tree` means exactly one thing: **this machine
supplies no tree of its own, so build one from the devices it declares and hand
it over with `-dtb`.** It is not a property of the board and it is not an
on/off switch for the device tree itself — every platform hands the kernel one
either way.

It lives on the emulation profile rather than the hardware record because it
describes the machine being emulated. A machine's device list states what that
machine models, which is often a subset of what the board has; a tree built from
the board's list would name hardware the machine does not have, and the
bootloader would then probe devices that return zero. Two lists, two machines,
no sharing.

A machine that supplies nothing and is not given a generated tree fails by name
(`BOOT_AARCH64_DT_REFUSED: the machine supplied no usable device tree`). It is
never handed an invented one: an invented tree is a claim nothing can check, and
a wrong one surfaces as a hang far from its cause.

## Contents

**One contract per machine, tailored to it.** There is no union schema and no
common subset every machine must declare. A BCM2837 tree describes PL011, sdhci
and a mailbox; a `virt` tree describes GIC and virtio-MMIO. Flexibility across
machines comes from the vocabulary being open and the reader tolerant, not from
the schema being rich enough to describe everything.

A generated tree is a projection of the machine's emulation profile — never a
hand-written document, and never containing a node for a device the machine does
not have:

- `/` — `model` and `compatible` naming the machine, `#address-cells = 2`,
  `#size-cells = 2`
- `/memory@<base>` — `device_type = "memory"`, `reg` with the RAM base and size
- `/cpus` with one `cpu@N` per declared hart, `device_type = "cpu"`, and
  `compatible` from the profile's `cpu`
- `/soc` with one `<model>@<base>` node per device the profile declares,
  carrying `compatible = <model>`, `reg = <base size>`, and `interrupts` **only
  where the profile declares an interrupt**

A device that raises no interrupt has no `interrupts` property, and a consumer
must read that absence as the fact rather than supplying a default. A device
with no window describes a function rather than a region and gets no node.

**No placeholder nodes.** A node for a device that does not exist is a lie the
driver probe then has to defend against, and it defeats the point of tailoring.
Driver selection follows the tailoring: a machine without a device has no node,
its driver never loads, and that is correct behaviour rather than a degraded one.

A peripheral states its own window. A peripheral that states a base without a
size is refused rather than given a default, because one window must have one
declaration and a second source could disagree with the first. A peripheral with
no base describes a function rather than a window and is omitted.

A peripheral whose registers start part way into a page is the one case where a
window is described at two granularities, and the two are tied together rather
than left to agree by inspection. Its exact window lives in `hardware.devices`
and becomes its tree node; the page it sits in lives in `mmio_devices`, because
the MMIO inventory that modules match and claim against must be page aligned and
page sized, so it cannot state a sub-page window. The build refuses a record
where such a window falls outside every declared MMIO range, or where one
declared range holds two windows — one claim would then cover two devices. A
driver claims by the inventory entry and takes its register window from the
tree; it must not add a hardcoded offset of its own.

`boot_cpuid_phys` is 0: the bootloader starts on the primary hart.

## Consumer rules

Consumers must tolerate unknown properties and nodes rather than rejecting them,
so a future platform's extras are data rather than a kernel change. A consumer
that finds a malformed header must report it; a tree that parses but describes no
devices is the failure mode with the worst diagnosis cost, because it presents as
a machine with nothing attached.

The kernel's inventory is the first such consumer, and it is what makes the tree
load-bearing rather than decorative. The platform record declares *which*
peripherals a platform carries, as identities - a bus and a compatible string.
The machine's tree is the authority for *where* they are. Admission resolves each
declared identity through the tree and publishes the result once; the kernel is
not compiled with any device address. A declared peripheral the tree does not
describe is refused by name, and there is no fallback array to reach for, because
a window the kernel maps but the machine does not implement faults a long way
from its cause.

The bootloader resolves the same identities through the same tree, so the two
agree by construction rather than by both being edited together.

The bootloader initialises its console from the record before the tree can be
parsed, since a bad tree has to be reported somewhere. It then compares the
discovered console window against the one in use and fails by name on a
divergence, so the two cannot drift silently.

## Testing the tree handoff, and how to get it wrong

QEMU's **ELF** loader does not set `x0`; its **raw-binary** loader does. Loading
the bootloader with `-kernel <elf>` therefore reports `x0 = 0` whether or not
`-dtb` is supplied, and makes it look as though `-dtb` never reaches the
bootloader. It does: with the `.bin` that `cargo xtask` and real firmware use,
`x0 = 0x48000000` on `-machine virt` both with and without `-dtb`.

Test the handoff with the binary, not the ELF, or you will conclude the
mechanism is broken when your test method is.

QEMU also validates a `-dtb` blob's magic itself and refuses to start on a
corrupt one (`load_device_tree: Unable to copy device tree into memory:
FDT_ERR_BADMAGIC`), so a hand-built garbage blob cannot exercise the bootloader's
own validation. To test a refusal, supply a *valid* tree that lacks what the
loader needs - for example a 174-byte root node with no console - and check that
the loader names the refusal and does **not** print `BOOT_AARCH64_READY`.

## Where the clock comes from, and why it is not in the tree

AArch64 states its counter frequency in a system register, not in the device
tree: `mrs {}, cntfrq_el0` is the authoritative read, and a machine that leaves
it zero is refused rather than given a guessed rate. The `clock-frequency` that
`-machine virt`'s tree does declare belongs to `apb-pclk`, the 24 MHz peripheral
bus, and is not the generic counter.

This matters because the absence looks like a gap. It is not: the frequency is
stated somewhere other than the tree on this architecture, so it needs no tree
entry and no `emulation` field. Do not add one.

## Validation, and what it must not do

Shared validation checks the envelope and what it consumes: magic, header size,
alignment, that the blob lies within its reservation, and the properties of
nodes it actually reads. It must **never** reject a machine for lacking a node
it does not have. A missing capability is a named error at the *consumer* — no
framebuffer device found, so no framebuffer — not a schema rejection of the
whole tree.

## Cross-checking the declaration against the machine

The record is no longer the runtime contract, so comparing our declared devices
against the tree the machine actually supplied is the only thing tying the
declaration to reality. A divergence is reported by name.

Under `-machine raspi3b` the emulated tree over-claims relative to what QEMU
models — no HVS, no V3D, no real DMA pacing — so the cross-check must
distinguish *"the machine's tree disagrees with our record"* from *"the emulated
machine models a subset of the board"*. The second is expected and is the list
of things only real hardware can test.

## Verification

`tools/mac-build/src/devicetree.rs` carries host tests that parse the emitted
blob with a parser separate from the emitter and assert the memory region, the
CPU nodes, every declared device window and the header's offsets. The test parser
uses literal token values from the Devicetree Specification rather than the
emitter's constants, so an emitter writing the wrong token is not confirmed by a
parser repeating the mistake.

## Remaining gates

- The kernel-side consumer is implemented and is the same code on every
  architecture: `machine::platform_devices` resolves each declared peripheral
  identity through the machine's tree at admission. What remains unverified is
  its `aarch64` half: no bootable `aarch64` machine is installed here, so that
  path is compiled but has never run.
- The kernel resolves a peripheral's interrupt by finding the controller its
  `interrupts` specifiers are written for and reading that controller's
  `#interrupt-cells`. On `qemu_riscv` the one declared peripheral declares no
  interrupts, so the resolution runs but the phandle path it contains is not
  exercised by any boot.
- `-machine raspi3b` accepts a `-dtb` and that path is wired, but the generated
  blob is not yet produced as a `dist/` artifact for the invocation to name, so
  the emulated run of that machine is not yet exercised end to end.
- A QEMU run proves behaviour on QEMU's machine, never on the board. The
  emulated tree contains no HVS, no V3D and no real DMA pacing, so no boot-check
  on that platform can validate them.
- The record declares both `hardware.devices` and `mmio_devices`. The generator
  reads `emulation.devices`; the kernel's inventory reads the identities from
  `mmio_devices` and every address, size, interrupt and flag from the tree.
- A numeric disagreement between a record's declared address and the machine's
  tree is not detected at runtime, because the declared address is no longer
  compiled in. `devices::check` validates it at build time, and the tree is the
  runtime authority; the build-time cross-check against a real tree is what
  would close the gap, and it is not implemented.
