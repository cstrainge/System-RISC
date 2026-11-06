# Device tree

## Each machine supplies its own tree — the governing rule

A device tree is a claim about the machine in front of you, so the machine must be
the one that makes it.

* On the physical board, the firmware's tree describes the board: correct for the
  variant, with the configuration's overlays already applied, because that is the
  machine that exists.
* Under an emulator, the tree the machine passes describes exactly what that
  emulator models — no more. An emulated board is not the silicon: it lacks blocks
  the board has, models some as placeholders that swallow writes, and paces DMA
  differently or not at all.

Those are two different machines, and a single fixed description can only be right
for one of them. Handing an emulated machine the board's tree tells the kernel
about hardware that is not there; the kernel then probes, hangs or reads garbage,
and a check that runs there stops predicting the board. Handing the board the
emulator's tree loses peripherals that do exist. Supplying each from its own
machine is right by construction.

**The record's `devices` list is therefore a declaration, not the runtime
contract.** It is used for validation — including the cross-check below — and for
module selection and the claimable inventory. The tree the machine passes is what
the kernel walks. A difference between them is a record-accuracy question to be
resolved, not a discrepancy to be "fixed" by editing the record to match whichever
machine was looked at last.

**The generated path remains, and remains legitimate**, for a machine that passes
no tree and for checks that deliberately want a fixed description.

**One record, one machine.** `platforms/raspi3b.json` describes the board and
declares no emulation; the emulated aarch64 machine is its own record,
`platforms/qemu_arm64.json`. The two were one record while an emulated Pi profile
existed, and that arrangement went with the profile: a record serving two machines
states facts that are true of one and false of the other, and a reader cannot tell
which is which. Where the two disagree, each machine states its own fact.

## Cross-checking the record against the machine

When the machine's own tree is available at build time — the board's tree is
copied into the image's boot partition, so it is — the record is compared against
it, node by node, by `compatible`: address, length, and whether an interrupt is
declared.

It is a **warning, not a refusal**: it is a new check against a source this
project has not yet validated, and a hard failure on first contact would block
work on a false positive. It reports both values so a difference can be resolved.

It compares the machine's tree **pre-overlay**, and that is a limitation worth
stating because it is easy to get backwards. The file in the boot partition is the
*base* tree the firmware publishes; `start.elf` applies the `config.txt` overlays to
a copy in memory and hands the result to whatever it starts. So the file on the card
is the machine before its configuration and the tree the kernel walks is the machine
after it. Comparing the file still checks what matters — addresses, lengths and
interrupts do not change under an overlay, only `status` does.

Two translations make the comparison meaningful, and both were found by running it
rather than by reasoning about it:

* **Vocabulary.** The record names a part as its own inventory identity
  (`bcm2835-mailbox`); a tree names it by `compatible` (`brcm,bcm2835-mbox`). Each
  peripheral therefore declares its tree `compatible`, defaulting to the inventory
  name, and the comparison is between compatibles.
* **Address view.** A board's tree states peripherals in the VideoCore's bus view
  (`0x7E000000`) and the record states them in the ARM view (`0x3F000000`) of the
  same registers. The comparison translates between the two, because comparing
  them raw reports every device as a divergence.

A size difference is reported only when the machine's block is **larger** than the
record's window: the record states the page a driver claims and the tree states the
register block inside it, so the record is normally the larger, and only the other
direction means the record under-declares a device.

Interrupts are compared where the form can be read as a number, and left alone where
it cannot. The record states a flat ARM interrupt number; a tree states an `armctrl`
`<bank bit>` pair, and only the controller's bank 2 has a rule this project has
calibrated: a node there raises ARM interrupt `bit + 32`. Four points establish it —
`<2 17>` is GPIO, ARM 49; `<2 25>` the PL011, ARM 57, which the record declares from
the datasheet; `<2 29>` the RNG, ARM 61; `<2 30>` the SD controller, ARM 62. Banks 0
and 1 state a different numbering, so the pair is reported for a human to read rather
than decoded by guesswork. The rule caught a fourth record error on its first run:
the inventory's sdhost interrupt, which is 56 and was written as 30.

An interrupt is looked for in the node *and in the nodes inside the window the
record claims*, because a record claims a page and states one line for it while a
tree states interrupts per functional node. The auxiliary block claims a page and
states no interrupt itself; its UART and SPI ports sit at `0x7E215040` and above,
inside the same page, and state `<1 29>`.

A tree this build generates states the flat number under a platform-owned
`platform,arm-interrupt`, never under `interrupts`. The standard property means "as
this node's interrupt parent spells it", and a tree with no interrupt controller node
has no parent whose encoding could be written there — so writing the number under it
would be a claim about a binding that is not in the file.

`status` is treated as the machine's configuration of software it would itself run,
not as a statement about hardware, and a difference is therefore never a warning:
a peripheral the firmware does not enable is one no Linux driver would probe, while
its registers sit where the record says and a bare-metal driver configures it
directly. Those findings are printed, in a section of their own, with the record's
own note beside each — and a record cannot mark a device `declared` without stating
why, so the section cannot become a place where differences are filed and forgotten.

## What it found on the board's own tree — 2026-11-05, and again after the fixes

Run against `bcm2710-rpi-3-b.dtb` from the firmware on a real card, pre-overlay, by
`cargo xtask image --platform raspi3b`. Eleven declared devices; **one warning and
five accounted-for differences**. Before the work below it was eight warnings that
looked like eight unexplained mismatches.

### The three that were the checker's fault, not the record's

| Symptom | Cause | Fix |
|---|---|---|
| `pl011` reported absent from a tree that contains it | The node's `compatible` is the list `"arm,pl011-axi", "arm,pl011", "arm,primecell"`, and the reader kept only the first string. | The reader keeps every string, and matches on any of them. |
| Every window reported as a divergence | The board states the bus view, the record the ARM view. | `bus_to_arm` translates before comparing. |
| `bcm2835-aux` reported as claiming an interrupt the tree lacks | The interrupt is on the nodes inside the page, not on the node the record names: the mux at `0x7E215000` is 8 bytes and its UART/SPI ports at `0x7E215040`+ state `<1 29>`. | The interrupt is looked for across the window the record claims. |

A fourth reader defect contributed to all of the above: a node record was created when
`compatible` appeared, so a property written before it attached to the previous node,
and several devices came back with a window of `0x0`. A node is now created when it
opens, and a property lands on the node that is open.

### The one warning that remains, and why it is not silenced

| Device | Finding |
|---|---|
| `sdhci` | The tree disables `mmc@7e300000` and enables **`mmcnr@7e300000`** — the same address, the same `compatible` — as a non-removable host with a `brcm,bcm4329-fmac` wireless child. The machine's own description therefore assigns these registers to the wireless module, and puts the card on `sdhost@7e202000`, which it marks `okay`. |

Against that stands the recorded board run: a self-test binary on this board read the
user's microSD card's master boot record through `0x3F300000`, and the partition
offsets prove the bytes came from the real card rather than from our own image
(`docs/HARDWARE-OBSERVATION-AARCH64-SELFTEST.md`).

**The hardware run decides it.** A `status` in a device tree states which drivers the
firmware was configured to bring up; it does not state that a bare-metal reader cannot
drive the block, and this project is a bare-metal reader. The card returned its own
sector zero through the Arasan registers, so that is the controller the silicon puts it
on and the one the platform drives. `sdhost` stays unused with no window granted. The
warning remains because the difference between the record and the machine's tree is
still real and a reader should see it — but it is now a difference the record explains,
not one it leaves open. The firmware's arrangement is coherent for its own boot; what
is settled is only what this project must do.

### The five accounted-for differences

Printed with the record's own words beside each, in a section that is not the warning
block:

| Device | Why it is expected |
|---|---|
| `spi0`, `pwm`, `i2s`, `v3d` | The firmware disables a peripheral it is not using, unless `config.txt` asks for it. Its `status` is a statement about the drivers it would run, not about register files that are present and reachable — our driver configures the block and its pins itself. `pwm`, `i2s` and `v3d` additionally carry `disposition: declared`: the record names them because the board carries them and nothing claims them, which is the tailored-tree rule, and the record has to say so to use it. |
| `sdhci` | Also in this section for its `status` alone, with the record's note pointing at the contradiction above. |

`pwm`, `i2s` and `cprman` declare no interrupt in the record and none is stated for
them in the tree, so there is nothing to compare; the count is reported in the
success line rather than as a finding, so the agreement is visible without looking
like a difference.

### Why the check earns its place

It was built because the record had been wrong three times in one day — an SPI
interrupt, three I2C controllers sharing one, a DMA channel set — and every one was
found by a person re-reading a datasheet. On its first honest run it found a fourth:
the inventory's sdhost interrupt is **56**, not 30, derived from the tree's `<2 24>`
under the calibrated bank-2 rule. It also found that our own generated tree carried no
interrupts at all, so a kernel walking it could not have learned one; the generator
now states them.

## Flow (the generated path)

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

- The kernel-side consumer is not implemented for `aarch64`. The bootloader
  consumes the tree today; the kernel port is what makes the second consumer
  real, and until it exists the tree is decorative on that platform.
- `-machine raspi3b` accepts a `-dtb` and that path is wired, but the generated
  blob is not yet produced as a `dist/` artifact for the invocation to name, so
  the emulated run of that machine is not yet exercised end to end.
- A QEMU run proves behaviour on QEMU's machine, never on the board. The
  emulated tree contains no HVS, no V3D and no real DMA pacing, so no boot-check
  on that platform can validate them.
- The record declares both `hardware.devices` and `mmio_devices`. The generator
  reads `emulation.devices`; the kernel's MMIO inventory reads `mmio_devices`.
  The cross-check above is what will tie them together.
