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

**This record describes two machines.** `platforms/raspi3b.json` serves both the
physical board and the emulator's model of it — one record, two machines. That is
coherent only because the tree comes from the machine; where the record itself
states a fact the two disagree about, the record is stating the board's fact and
the emulated run is the one that has to be read with that in mind.

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

Against that stands a recorded board run: a self-test binary on this board read the
user's microSD card's master boot record through `0x3F300000`, and the partition
offsets prove the bytes came from the real card rather than from our own image
(`docs/HARDWARE-OBSERVATION-AARCH64-SELFTEST.md`). Two pieces of evidence that cannot
both be right about the same silicon, and nothing in this repository decides between
them. The record still selects sdhci because it is the only path with a driver and the
only path with positive evidence; the warning stays because the disagreement is real
and unresolved. What would settle it is a board run probing both controllers and
reporting which one returns the card's sector zero.

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
platforms/<name>.json  →  generated blob  →  bootloader  →  kernel
        (author)          (build time)        (consumer)    (consumer)
```

The platform record is the single source of truth. Nothing downstream hardcodes
a peripheral address that the record already declares, so a record edit changes
every consumer at once instead of requiring a search.

The blob is generated at build time and linked into the bootloader image in a
read-only section, so nothing is allocated at runtime and the handoff is a
link-time address.

## Declaration

A record states which kind of tree its board has:

| `hardware.device_tree.kind` | meaning |
|---|---|
| `generated` | the build emits the blob from this record |
| `firmware` | board firmware passes its own tree at entry |
| `file` | the platform ships a tree as a file |
| `none` | the platform describes no tree |

`mac_build::devicetree::generate()` refuses a platform that does not declare
`generated`, so the declaration is enforced rather than decorative. A board whose
firmware supplies its own tree must not silently receive a generated one.

## Contents

Emitted from the record:

- `/` — `model`, `compatible`, `#address-cells = 2`, `#size-cells = 2`
- `/memory@<base>` — `device_type = "memory"`, `reg` with the record's RAM base
  and minimum size
- `/cpus` with one `cpu@N` per declared hart, `device_type = "cpu"`
- `/soc` with one `<model>@<base>` node per peripheral the record declares,
  carrying `compatible = <model>` and `reg = <base size>`

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
  real.
- `-machine raspi3b` passes no firmware tree, so the `firmware` path is
  unexercised on this host.
- The record declares both `hardware.devices` and `mmio_devices`. The generator
  reads only `hardware.devices`; the kernel's MMIO inventory reads
  `mmio_devices`. The two listings describe the same windows and nothing yet
  checks that they agree.
