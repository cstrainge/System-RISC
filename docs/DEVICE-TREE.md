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

It compares the machine's tree **post-overlay**, because that is also what the
kernel sees, so a peripheral an overlay disables appears as an expected finding.

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

Interrupt **presence** is compared; interrupt **numbers** are not. The record states
a flat number from the SoC's peripheral interrupt list and a tree states an `armctrl`
`<bank bit>` pair, so they are not the same quantity. A translation between them
would have to come from the interrupt controller's own bank layout — deriving it
from the two sources being checked would make the check agree with itself and prove
nothing. The board's tree and this record do happen to line up three ways
(`<1 10>` for the 3D block against a declared 10, `<1 16>` for DMA against 16,
`<2 29>` for the RNG against 61), but that correspondence is an observation to be
explained by later work, not a rule this check is entitled to assume.

A node the firmware marks with a `status` other than `okay` is reported too: the
block is in the tree but the firmware does not bring it up, so a record that
declares it as the platform's device disagrees with the machine's configuration.

## What it found on the board's own tree, 2026-11-05

Run against `bcm2710-rpi-3-b.dtb` from the firmware on a real card, post-overlay.
Ten of the eleven declared devices agree on address after translation — including
the 3D engine at bus `0x7EC00000` / ARM `0x3FC00000`, which independently confirms
the corrected figure. The findings:

| Device | Finding |
|---|---|
| `pl011` | The tree has **no** `arm,pl011` node at all. The only node at `0x7E201000` is `bluetooth`, and the console on this board is reached through the firmware's own configuration of UART0, not through a node a kernel can find by this compatible. Our generated tree carries the node, so this divergence is ours to resolve — but not by deleting the device. |
| `sdhci` | The tree has `brcm,bcm2835-mmc` at `0x7E300000` with `status = "disabled"`, and `brcm,bcm2835-sdhost` at `0x7E202000` with `status = "okay"`. **The recorded hardware run read the card through the Arasan block.** Both are evidence and neither is dismissed; see `docs/PI-HARDWARE-INVENTORY.md`. |
| `spi0`, `pwm`, `i2s`, `v3d` | Marked `status = "disabled"` by the firmware — the configured state of a board whose `config.txt` does not enable them. Not a hardware claim. |
| `bcm2835-aux` | The record declares interrupt 29; the tree's aux node states none. The number appears one level down, on `brcm,bcm2835-aux-spi@7e215080` as `<1 29>`. |
| `pwm`, `i2s`, `cprman` | Neither source declares an interrupt, which is what the record claims, so there is nothing to compare. Listed so the absence is visible. |

The `status` findings are the reason this check earns its place: the discrepancy
between the two SD controllers is exactly the kind of thing that would otherwise
have been discovered by a driver failing on a board.

The check exists because the record has been wrong three times in one day — an
SPI interrupt, three I2C controllers sharing one, a DMA channel set — and every
one was found by a person re-reading a datasheet. Arithmetic would have found all
three.

**It already found a fourth:** the tree this build writes carries `compatible` and
`reg` and no `interrupts` property, so a kernel walking our own generated tree
cannot learn a device's interrupt.

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
