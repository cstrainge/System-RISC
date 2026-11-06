# Device tree

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
