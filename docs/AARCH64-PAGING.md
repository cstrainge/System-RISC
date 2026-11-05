# AArch64 paging for System RISC

This document records what the AArch64 translation hardware requires, and the
rules this project follows when building tables for it. It is written before the
implementation so that the loader is built from a stated contract rather than
discovered by trial and error.

Facts here are the architecture's documented interface — register and descriptor
field positions and their meanings. They are not taken from any implementation's
source. No third-party code is transcribed into this project.

## Why this document exists

The first aarch64 boot MMU was written by inference and was wrong three times:

1. The EL3/EL1 transition was missing, so `SCTLR_EL1` did not control anything.
2. The 39-bit regime was assumed to be a four-level walk; it is three-level.
3. The identity map was installed at level-1 index 0, which is only correct when
   RAM begins at zero.

Each was invisible until a machine with different properties ran the code. The
read-back check described below exists because the third failure produced a
mapping that looked correct and was not.

## Translation regime

The platform uses a 39-bit virtual address space with a 4 KiB granule.

`T0SZ`/`T1SZ` = 25, because `64 - 25 = 39`. With a 4 KiB granule and a 39-bit
space the walk **starts at level 1**, not level 0: the top level covers 1 GiB per
entry. The leaves are therefore:

| Level | Leaf size | Entries per table |
| ----- | --------- | ----------------- |
| 1     | 1 GiB     | 512               |
| 2     | 2 MiB     | 512               |
| 3     | 4 KiB     | 512               |

A 39-bit space is canonical in two halves: `0x0000_0000_0000_0000` upward, and
`0xffff_ff80_0000_0000` upward. `TTBR0_EL1` serves the lower half and
`TTBR1_EL1` the upper. A range that straddles the hole has no valid encoding.

### `TCR_EL1` fields used

| Field   | Bits    | Value used | Meaning |
| ------- | ------- | ---------- | ------- |
| `T0SZ`  | 5:0     | 25         | lower half is 39 bits |
| `IRGN0` | 9:8     | `0b01`     | inner write-back, write-allocate walks |
| `ORGN0` | 11:10   | `0b01`     | outer write-back, write-allocate walks |
| `SH0`   | 13:12   | `0b11`     | inner shareable walks |
| `TG0`   | 15:14   | `0b00`     | 4 KiB granule for `TTBR0` |
| `T1SZ`  | 21:16   | 25         | upper half is 39 bits |
| `IPS`   | 34:32   | `0b010`    | 40-bit physical addresses |
| `TG1`   | 31:30   | `0b10`     | 4 KiB granule for `TTBR1` |

`TG0` and `TG1` encode 4 KiB differently: `0b00` for `TTBR0`, `0b10` for
`TTBR1`. They are not the same value.

`IPS` must cover the whole physical extent this machine uses. The record places
2 GiB at `0x4000_0000`, so addresses reach `0xC000_0000`; a 32-bit `IPS` only
just contains that and leaves no headroom for any device above 4 GiB.

Leaving `IRGN0`/`ORGN0`/`SH0` at zero makes page-table walks non-cacheable. That
is architecturally legal and functionally correct, but it is not the behaviour the
resident kernel expects, and it makes every walk a memory round trip.

## Descriptor formats

Every entry is 64 bits. Bit 0 is valid. Bit 1 distinguishes a table pointer from
a leaf:

| Bit 1 | Level 1, 2 | Level 3 |
| ----- | ---------- | ------- |
| 0     | table      | invalid |
| 1     | block      | page    |

A **table** descriptor holds the next-level table's physical address in bits
47:12.

A **block** (level 1, 2) or **page** (level 3) descriptor carries the output
address in bits 47:12 plus attributes:

| Field     | Bits  | Meaning |
| --------- | ----- | ------- |
| `AttrIndx`| 4:2   | index into `MAIR_EL1` |
| `NS`      | 5     | non-secure |
| `AP[2:1]` | 7:6   | access permissions |
| `SH`      | 9:8   | shareability: `0b11` inner shareable |
| `AF`      | 10    | access flag — **must be set or the first access faults** |
| `nG`      | 11    | not-global |
| `PXN`     | 53    | privileged execute-never |
| `UXN`     | 54    | unprivileged execute-never |

### The granularity constraint that broke the image window

A block or page descriptor's output address must be aligned to its own size. A
2 MiB block therefore requires the *physical* address to be 2 MiB aligned, and a
translation expressed with blocks requires the virtual and physical windows to be
**congruent modulo the block size**.

The System image window is not:

```
images.base            0x4093_0000           mod 2 MiB = 0x130000
system_image_base      0xffff_ffd0_0020_0000 mod 2 MiB = 0
```

The 0x130000 difference cannot be carried by a block descriptor. No choice of
block index makes this translation correct, which is why the image window is
mapped with **4 KiB pages** rather than 2 MiB blocks.

Mapping it with pages needs level-3 tables: one per 2 MiB of window, 4 KiB each.
The tables are bounded to the real image extent rather than the full declared
window, and live in the RAM below the loaded boot image.

The rule this yields: **block mappings only where the virtual and physical bases
are congruent modulo the block size; page mappings otherwise.** A loader may not
move a record's load address to suit its own granularity — the record owns that
address.

### `MAIR_EL1`

One byte per attribute index:

| Index | Value | Meaning |
| ----- | ----- | ------- |
| 0     | `0xFF`| normal memory, inner and outer write-back, read/write allocate |
| 1     | `0x00`| Device-nGnRnE |

Device memory is mapped with `AttrIndx` 1 and both `PXN` and `UXN` set: the
device aperture is never executable.

## Enabling translation

The sequence, with the ordering that matters:

1. Build the tables. Every table must be zeroed first; a stale entry is a valid
   descriptor pointing at arbitrary memory.
2. `DSB SY` — the table writes must be visible before the walker can read them.
3. Write `MAIR_EL1`, then `TCR_EL1`, then `TTBR0_EL1` and `TTBR1_EL1`.
4. `ISB` — the register writes must be complete before the next instruction.
5. Read `SCTLR_EL1`, clear bit 19 (write permission implies execute-never is
   cleared so W^X is not silently imposed), set bit 0 (`M`, enable translation),
   set bit 2 (`C`, data cache) and bit 12 (`I`, instruction cache). Write it back.
6. `ISB`.

The instruction that enables translation must itself be mapped by the tables
being enabled, at the address the program counter currently holds. On a machine
whose RAM does not start at zero this is the whole problem: the identity map must
be installed at the level-1 index that covers the RAM base, not at index 0.

## Checks that must exist

A mapping is a claim about hardware. It is verified by use, not by inspection:

- **Identity**: a value written through a physical address is visible through the
  same address as a virtual one.
- **Physmap**: a value written through a physical address is visible through the
  kernel-half physmap alias.
- **Image window**: a value written through the physical image window is visible
  through the window's virtual address.

Each is a sentinel write followed by a read-back comparison, reported on the
console as its own marker. A failing check reports the failure; it does not
continue as though the mapping were sound.

## Relationship to the RISC-V paging path

The two architectures this project supports are structurally the *same* shape:

| | Sv39 | AArch64, 39-bit |
| --- | --- | --- |
| Virtual address width | 39 | 39 |
| Walk | 3 levels | 3 levels |
| Leaves | 1 GiB, 2 MiB, 4 KiB | 1 GiB, 2 MiB, 4 KiB |
| Canonical halves | yes | yes |

What differs is only the *encoding*, never the geometry:

| | Sv39 | AArch64 |
| --- | --- | --- |
| Root selection | one `satp` | `TTBR0_EL1` + `TTBR1_EL1`, split by half |
| Descriptor bits | PTE `V/R/W/X/U/G/A/D` | valid, table/block/page, `AF`, `AP`, `AttrIndx`, `SH`, `PXN`/`UXN` |
| Attributes | inline in the PTE | indexed through `MAIR_EL1` |
| Accessed/dirty | `A`/`D` bits, may fault | `AF`; dirty handled in software |

So the level-index arithmetic, the leaf selection, the valid-range rule and the
congruence constraint are **geometry**, and belong in shared code driven by the
platform record. The descriptor encoding is **ISA**, and belongs behind the
selection point.

This is the same boundary `machine/page_layout` already draws, and it is why the
aarch64 encoder should be a peer of `mac-riscv-pages` implementing the existing
`Tables`/`Mapper` seam rather than a standalone table builder. The shared part is
index computation; the per-architecture part is turning an index, a physical
address and a set of permissions into 64 bits.

The current shared paging code still names `mac_riscv_pages::{Entry, Mapper,
Tables}` directly. That is the remaining leak this section describes, and moving
the aarch64 encoder in behind the same seam is what closes it.

## Interface both encoders must provide

The geometry is shared, so the two architectures must present one interface to the
code above them. Shared paging builds and admits address spaces; it must not be
able to tell which encoder it is talking to.

The seam already exists in `mac-kernel-model::pages`:

```rust
pub trait PageLayout
{
    fn geometry() -> PageGeometry;
    fn leaf_sizes() -> &'static [u64];
    fn valid_virtual_range(range: AddressRange) -> bool;
}

pub trait Architecture: PageLayout
{
    type InvocationGate: InvocationGate;
    type PageTable;
    type TrapFrame;
}
```

and in the table builder:

```rust
pub trait Tables
{
    type Error: core::fmt::Display;
    fn allocate_zeroed(&mut self) -> Result<PhysicalAddress, Self::Error>;
    fn load(&self, table: PhysicalAddress, index: usize) -> Result<u64, Self::Error>;
    fn store(&mut self, table: PhysicalAddress, index: usize, entry: Entry)
             -> Result<(), Self::Error>;
}
```

Both architectures must satisfy these with the same *meaning*, not merely the
same signatures:

| Operation | Sv39 | AArch64 |
| --- | --- | --- |
| `allocate_zeroed` | one 4 KiB frame, zeroed | identical |
| `load` | the raw 64-bit entry | identical |
| `store` | encode then publish | identical |
| root selection | `satp` | `TTBR0_EL1` / `TTBR1_EL1` |
| flush | `sfence.vma` | `tlbi` + barriers |

Three consequences for the implementation:

1. **`Entry` must not be RISC-V's.** Shared code currently names
   `mac_riscv_pages::Entry`. The encoded-descriptor type has to move behind the
   selection point — an associated type of the architecture, or a
   `machine::page_layout::Entry` — so that shared code stores a descriptor it did
   not encode and never names an ISA.
2. **`Mapper` must be generic over the encoder, not over Sv39.** The level-index
   arithmetic and leaf selection are geometry and stay shared; only the encoding
   step is per-architecture.
3. **`root()` and `activate()` stay per-architecture** and already are, in
   `machine/mmu`. Admission asks `validate_root`/`admission_interrupts_clear`,
   which are per-architecture and already exist.

A peer crate for AArch64 mirroring `mac-riscv-pages` — same public shape, its own
descriptor encoding — is the shape this implies. The alternative, a second
paging implementation beside the shared one, is the duplication this document
exists to prevent.

## The supervisor handoff

Starting System means satisfying four contracts, all read from the ABI rather
than inferred.

### `BootInfo` (`mac_abi::BootInfo`, 176 bytes, layout asserted)

| Field | Source |
| ----- | ------ |
| `header` | `Header::for_type::<BootInfo>()` |
| `ram_base`, `ram_size` | the first RAM bank |
| `log`, `disk_info`, `read_sector`, `write_sector`, `flush_disk`, `storage_context` | the loader's disk callbacks |
| `startup_disk`, `flags`, `startup_partition_start` | the boot volume |
| `ram_regions`, `reservations` and their counts | the checked memory account |
| `display_scratch` | the reserved framebuffer extent, or zero for a headless boot |
| `execution` | the `boot::execution::Execution` record for the load plan |
| `cpus`, `cpu_count` | sorted enabled hart IDs |
| `monitor` | the monitor `Directory` |
| `monitor_layout` | must equal `boot::monitor::LAYOUT_FINGERPRINT` |

The kernel refuses the image if `monitor_layout` does not match, so the monitor
is not optional and cannot be stubbed with a zero.

### The monitor `Directory`

```
entries:     *const Entry     per hart: { hart, msip, storage: BootMemoryRange }
count:       usize
stack_bytes: u64
timebase_hz: u64              the real /cpus frequency, never a hardcoded clock
image:       *const BootImage
```

`monitor::admit` validates every isolated control extent against the checked
memory view before borrowing the atomic cells, so the storage ranges must be
real, page-aligned reservations.

**An ISA-specific field sits in this shared ABI.** `Entry::msip` is a RISC-V
memory-mapped software-interrupt register. AArch64 has no equivalent — its
interprocessor interrupts are GIC SGIs — so an AArch64 loader can only publish
zero there, and the AArch64 monitor must drive IPIs through the GIC instead. That
is a property of the ABI, not a choice, and it should be resolved deliberately
rather than by writing zero and hoping the kernel never reads it.

**Resolved: the field is the platform's signalling target, and it is renamed.**
`Entry::msip` and `Control::msip` become `signal`, documented as *this hart's
inter-processor signalling target, encoded by the platform and opaque to every
consumer that does not implement signalling for that platform*. The encodings are
each machine's own and neither is privileged:

| platform | `signal` holds | the monitor raises an IPI by |
|---|---|---|
| RISC-V | the CLINT `msip` cell for this hart | writing the cell, which sets SSIP |
| AArch64, GICv2 | the `GICD_SGIR` address for this machine | writing `GICD_SGIR` with this hart's target list |
| AArch64, GICv3 | the SGI number and affinity, packed | writing `ICC_SGI1R_EL1` |

`monitor::validate` therefore stops requiring a CLINT window. It requires that
the platform's *own* signalling route is present and consistent with the
published values: a RISC-V record must supply the CLINT window that contains
every `signal`, and an AArch64 record must supply the interrupt controller
window it declares. A platform whose signalling needs no MMIO route states that,
and the check is that it states it — not that it declares a CLINT it does not
have. The observable behaviour is unchanged on RISC-V: the same cells, the same
SSIP injection, the same admission.

**The secondary's translation root becomes two registers.**
`SecondaryStart::root_register` carries one value, which is right for RISC-V
(`satp`) and insufficient for AArch64, whose secondary needs `TTBR0_EL1` *and*
`TTBR1_EL1` — two roots, because the kernel half and the physmap are addressed
through different base registers. It becomes `root_register` and
`root_register_aux`, documented as the platform's primary and secondary
translation roots: RISC-V publishes `satp` in the primary and zero in the
secondary; AArch64 publishes `TTBR0_EL1` and `TTBR1_EL1`. Everything else the
secondary needs to re-enter — `TCR_EL1`, `MAIR_EL1`, the vector base — it
rebuilds from the same record the primary used, so it is not passed.

The ABI is unreleased, so this is a rename and a field addition rather than a
migration, and every producer and consumer is rebuilt together.

### What the loader must still do

1. Read the System code resource from the resource fork and decode it with the
   native image format, not an ELF reader: `mac_formats::native::image::decode`
   plus `LoadPlan`, both already shared and already used by the RISC-V loader.
2. Map the rest of the kernel half — stacks, heap, the definition and application
   bases — not just the image window.
3. Build `BootInfo` and the monitor `Directory` in reserved RAM.
4. Drop to EL1 and branch to System's virtual entry with the `BootInfo` pointer.

Only step 2 is currently started, and only for the image window.

## Open items

- The image window is currently mapped with 2 MiB blocks and the read-back check
  reports failure, which is correct. It must move to 4 KiB pages as above.
- `TCR_EL1` currently leaves `IRGN0`/`ORGN0`/`SH0` at zero and `IPS` at 32 bits.
  Both should be corrected in the same pass.
- The kernel half needs the rest of System's windows — stacks, heap, the
  definition and application bases — mapped before handoff, not just the image.

## Where the port stands, and what blocks it

Everything below `BOOT_AARCH64_READY` is unbuilt. The loader reaches it, finds
and blesses System, and idles; System has never started under AArch64. The
remaining work, in the order it must be done:

1. **The `signal` rename and the second translation root**, per the two
   decisions recorded above. Unreleased ABI, so producers and consumers are
   rebuilt together and no compatibility path is added.
2. **`BootInfo` construction.** The loader decodes the System code resource with
   `mac_formats::native::image::decode` and `LoadPlan` — the same shared path the
   RISC-V loader uses — then builds `BootInfo` and the monitor `Directory` in
   reserved RAM, per the field list in this document. `monitor_layout` must
   equal `boot::monitor::LAYOUT_FINGERPRINT`; the kernel refuses the image
   otherwise, so the monitor is not optional.
3. **The AArch64 monitor.** It does not exist. It is the largest piece, because
   it owns what the RISC-V M-mode monitor owns — per-hart control blocks, the
   isolated control extents, wake and stop delivery, and the secondary start
   protocol — but must do it from a higher privilege level with a GIC rather
   than a CLINT. Its storage extents are checked against the same memory account
   the kernel uses.
4. **The EL1 jump.** Branch to System's virtual entry with the `BootInfo`
   pointer. The acceptance is a *System-side* line, not another loader line.

`PLATFORM_MMIO_DEVICES` deletion rides with step 2 or after it. Its `flags` do
not all come from a tree: `MMIO` is implied by the node, `COHERENT_DMA` is the
tree's `dma-coherent` property, and the match key is built from `compatible` —
but `RESIDENT_OWNED` is policy and comes from the module's own interface
declaration. A reader looking for it in a device tree will not find it, and
should not: hardware facts come from the tree, policy from the declaring
component.

Nothing in this section is verified. The toolchain on the machine that produced
the measurements above lost its precompiled standard library, so no crate
compiles and none of steps 1–4 has been built or run.
