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

## Open items

- The image window is currently mapped with 2 MiB blocks and the read-back check
  reports failure, which is correct. It must move to 4 KiB pages as above.
- `TCR_EL1` currently leaves `IRGN0`/`ORGN0`/`SH0` at zero and `IPS` at 32 bits.
  Both should be corrected in the same pass.
- The kernel half needs the rest of System's windows — stacks, heap, the
  definition and application bases — mapped before handoff, not just the image.
