# AArch64 boot state

Where System's AArch64 port stands, what is proven, and what is not. Written to
be read instead of re-derived: the list of things already ruled out is as useful
as the list of things fixed.

## Position

`cargo xtask boot-check --platform qemu_arm64` runs two cases.

- `no-disk` passes.
- `raw-hfs-b` reaches `TRAPS_READY` and then faults inside `start_services`.

The markers printed before the fault, in order:

```
BOOT_AARCH64_EL1_READY
BOOT_AARCH64_MMU_READY: 39-bit three-level translation active
BOOT_AARCH64_PHYSMAP_READY
BOOT_AARCH64_KERNELMAP_READY
BOOT_AARCH64_MONITOR_READY
BOOT_AARCH64_IMAGE_LOADED
BOOT_AARCH64_HANDOFF
BOOT_AARCH64_ENTERING: System at EL1
CPU_TOPOLOGY / STACK_POLICY / PAGE_BANK
MMU_READY
SYSTEM_HEAP_READY
SYSTEM_STACK_READY
TRAPS_READY: resident ecall dispatcher ABI=1
```

AArch64 has never reached `DESKTOP_READY` in this environment.

`qemu_riscv` passes 19/19 and is the gate that matters for landing.

## Defects found and fixed

Each was found by measurement, not by reading.

**Leaf and table descriptors had bit 1 inverted.** AArch64 uses descriptor bit 1
to separate a table pointer from a leaf only at the page level. Above it the
meaning inverts: bit 1 clear is a block that maps its own size, bit 1 set is a
pointer to the next level. The encoder set the leaf bit on every size, so every
1 GiB and 2 MiB block was written as a page-style descriptor that hardware
followed as a table pointer. The decoder had the mirror error, so a real block
written by firmware was followed as a pointer rather than recognised as a
mapping. `crates/mac-aarch64-pages` now has three tests that round-trip every
leaf size and assert the bit is set only at the page level.

**A mapping could not be made inside a coarser one.** `Mapper::map` returned
`Conflict` when a covering leaf already existed, so whether a mapping succeeded
depended on which had been written first. It now splits the covering leaf on
demand, so granularity no longer depends on ordering.

**`cfg(systemrisc)` was declared and never emitted** by `system/build.rs`. Every
other guest crate emits it. All 795 `#[cfg(systemrisc)]` sites in `mac-system`
therefore compiled their host arm into the guest, on both architectures. The
visible consequence was `firmware_call` calling firmware function pointers
without the translation-root switch. Auditing every build script in the tree
found this as the only omission.

**`TTBR1_EL1` carried the ASID tag.** Without `FEAT_ASID2`, `TTBR0_EL1[63:48]`
is the ASID and `TTBR1_EL1[63:48]` is RES0; the two halves share one ASID for
the regime. Activation wrote the tagged value to both. The address now goes to
`TTBR1` clean and the tagged value to `TTBR0`.

**Logging switched translation roots on every line.** `serial!` routed through
`kernel::log` to the firmware callback, which is reached through
`with_firmware`. The switch could not report its own failures: a print inside it
recursed, and a fault during it was silent. `kernel::log` now writes to the
platform console, which needs no switch.

Two mistakes made during this work are recorded because the commits carry them:

- A diagnostic was removed with a replacement whose range spanned the `msr`
  itself, leaving `write_root` as a doc comment with no body. It compiled, and
  every root switch silently did nothing until it was found.
- An earlier commit attributed a change in fault class to that switch. The
  change came from an added TLB flush alone.

## The open fault

`raw-hfs-b` faults with:

```
SYSTEM_TRAP_NO_CONTROL hart=0x80000000 ESR=0x8600000f
  ELR=0x4009aad4 FAR=0x4009aad4
  VBAR=0xffffffd000201800
  TTBR0=0x5092a000 TTBR1=0x40007000
  SCTLR=0xc5183d
```

`ESR 0x8600000f` is an instruction abort taken to EL1: permission fault, level
3. Three facts hold together and none of them is wrong:

1. `TTBR0` at the fault is the firmware root, `0x5092a000`, read in the handler
   rather than inferred.
2. The level-3 leaf for the faulting address is executable by descriptor:
   `0x4009a743` — valid, leaf, access flag set, writable, `PXN=0`, `UXN=0`,
   `AttrIndx=0`. The neighbouring page `0x4008ffbc` reads `0x4008f743`,
   identical except for the output address, through the same level-2 and
   level-1 tables.
3. `SCTLR_EL1` is `0xc5183d`: MMU, D-cache and I-cache on, `EE` off, `WXN` = 0.
   Nothing in `SCTLR` forces a writable page execute-never.

A level-3 permission fault on a fetch, with the right root, an executable leaf
and `WXN` clear, is not explained by anything readable from inside the guest.

## Ruled out by measurement

Do not re-run these; each was tested and failed.

| Hypothesis | Why it is wrong |
| --- | --- |
| The RAM bank shadows the more specific firmware mapping | The region is mapped second and wins; the firmware leaf reads as a normal executable page. |
| `MappingPlan::next` picks a leaf that is too coarse | It checks both physical and virtual alignment in both directions and always has. |
| The image range is left uncovered by a bank block | The regions resolve through their own tables to correct pages. |
| The vector rejects a valid control block | `NO_CONTROL` tests only `cbz` on `tpidr_el1`; no magic, no hart match, no dereference. |
| The PC returns to the bootrom through `LR` | `x30` was `0x4008f180` at handoff and clearing it changed nothing but the address's position. |
| A stale translation survives the root switch | An explicit `dsb ishst; tlbi vmalle1; dsb ish; isb` before the call changes nothing. |
| `WXN` forces writable pages execute-never | `SCTLR_EL1` bit 19 is clear at the fault. |
| The declared firmware window is too short | The window is `0x40080000` + `0xb0000`, ending at `0x40130000`; the fault is `0xff8c` into it. |

## Instruments that do not work here

- `qemu-system-aarch64 -d mmu` emits nothing for this target. Only `-d int`
  produces output, and it shows the exception without the walk.
- `-singlestep` is too slow to reach the first serial output, so it cannot be
  used to test whether the abort is speculative.

## Named separately, not explained

`TPIDR_EL1` reads zero at the fault even though `SYSTEM_STACK_READY` reports
`control=0xffffffe010910000` published, and it held a valid pointer when the
vector was installed. This is a separate root cause and is not a consequence of
the mapping work.

## Not attempted, deliberately

A typed wrapper that makes a firmware pointer unusable without the switch. The
`cfg` fix makes the invariant hold by construction for every `firmware_call`;
the wrapper is the better end state and belongs after the boot is green.

## Baseline warning

Enabling the `systemrisc` cfg changed 795 branches in shared code on both
architectures. The `qemu_riscv` 19/19 established after that change is a new
baseline: the earlier green said only that those cases never reached the
divergent code. The two numbers are not comparable.
