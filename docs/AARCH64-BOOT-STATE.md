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
- The monitor's `info tlb` and `info mem` are both **attempted and
  unavailable**. Querying them over `-monitor tcp:...,server,nowait` after the
  guest has reached the fault returns `Command 'info tlb' not available until
  machine initialization has completed` — while `info status` on the same
  connection reports `VM status: running`. Both commands are therefore refused
  by this build for this target rather than merely inconvenient to reach, and
  the MMU's own view of the faulting translation cannot be obtained here.

## What the three contradicting facts imply

Everything measurable from inside the guest now agrees with itself, and three
facts contradict the level-3 permission verdict: the firmware root is active,
the leaf is executable by descriptor, and `SCTLR_EL1.WXN` is clear. Every
in-guest instrument that could break the tie has been tried and is either
unavailable or impractical, as listed above.

That combination is the signature of a defect that is not visible from inside
the guest: either an emulator modelling gap, or something only silicon will
show. This project already validates silicon-specific behaviour on the board
rather than in QEMU, so the correct next step for this fault is that route, not
another in-guest candidate. It is left as a named, bounded open defect.

## The no-switch probe: inconclusive, not a withdrawal

A probe mapped every execution region in both spaces, so the firmware window was
also mapped in the kernel space, and then called the storage callback with no
translation-root switch at all. Both temporary changes were reverted.

| configuration | TTBR0 | result |
| --- | --- | --- |
| switch on (shipped) | `0x5092a000` firmware root | `ESR 0x8600000f` at `0x4009aad4` |
| switch off (probe) | `0x1000040007000` kernel root | `ESR 0x8600000f` at `0x4009aad4` |

**This does not exonerate the switch, and must not be recorded as doing so.**
The probe did not verify that the kernel-space leaf for the faulting address
became executable. The RAM bank is mapped before the execution regions and may
still own those pages, in which case the no-switch call ran against a
read/write/no-execute page and a level-3 permission fault is exactly what that
produces — expected, and no evidence about the switch either way. Both
configurations faulting looks like a shared cause only if the probe actually
made the page executable, and that was not shown.

A clean version of this test would need a third root mapping only the firmware
window, the image and the stack, with no RAM bank in it, so that an executable
leaf cannot be shadowed. That is a second space builder rather than a small
change, so it was not attempted; the ambiguity is recorded instead of papered
over. The switch-on fault is still the open puzzle.

## Dropping WRITE changed the fault, and what that does not prove

Two single-variable experiments found it.

**Not CPU-model-specific.** The failing case was run unchanged on `-cpu
cortex-a72`, `-cpu cortex-a53` and `-cpu max`. All three produce an identical
fault: same ESR, same ELR, same FAR, same registers. So this is not a
`cortex-a72` modelling gap and the CPU model is eliminated as a variable.

**Write-and-execute is refused even with `WXN=0`.** The firmware window's leaf
was `AP[2]=0`, that is writable and executable — a combination no shipping OS
produces, because Linux maps kernel text read-only-executable and honours
`EFI_MEMORY_RO`/`XP` rather than inventing permissions. Dropping `WRITE` from
the region's flags, so the leaf becomes `AP[2]=1` read-only-executable, changes
the fault completely:

```
before:  ESR=0x8600000f  ELR=0x4009aad4  instruction abort, permission, level 3
after:   ESR=0x96000006  ELR=0x4009ab08  data abort, translation, level 2
                                       FAR=0x524301d0
```

`EC` moved from `0x21` (instruction abort) to `0x25` (data abort), `ELR` is now
*past* the address that previously faulted, and the new fault is an ordinary
translation fault on a data access — the firmware trying to write to a window
that is now read-only, which is exactly what making it read-only would produce.

So the instruction fetch succeeds once the page is read-only, on this boot.

**What this does not establish — and it was claimed too strongly.** On
2026-11-07 the `raspi3b` bootrom was audited and found to map all of RAM
writable and executable with `SCTLR_EL1.WXN` cleared, in exactly the
configuration described above. QEMU executed that mapping without complaint and
reached its terminal marker (`BOOT_AARCH64_PROBE_FAILED`, no card attached).
That is the same TCG translation path, on `cortex-a53` — one of the three CPU
models already tested here — so a writable page *was* fetched from without
fault.

The consequence is that "QEMU's TCG refuses to execute a writable page" is
neither a QEMU-wide nor a TCG-wide property: it did not reproduce. What the
`qemu_arm64` experiment establishes is narrower — that changing that leaf from
writable-executable to read-only-executable turned an instruction abort into a
data abort past the faulting instruction, so the fetch began to succeed. Why
the same permissions faulted on one boot and not the other is **unexplained**,
and the record says so rather than carrying a mechanism it has no evidence for.
Untested candidates, none assumed: a difference in the leaf's other attribute
bits when `AP[2]` changes; a stage-1 interaction with privilege level or `PAN`;
or something in how each boot reaches its first fetch from a writable leaf.

**What the fix rests on instead.** Code read-only-executable, data
writable-non-executable, `WXN` set rather than cleared. That is what every
shipping OS does after enabling the MMU, and it is correct whether or not any
emulator requires it. The fault that disappeared is evidence for the change,
not its justification — a distinction worth keeping, because a fix justified
only by an unreproduced observation is a fix the next person undoes when they
fail to reproduce it.

**None of this is a statement about silicon.** `cortex-a72`, `cortex-a53` and
`max` are all TCG CPU models built on the same translation code, so an
identical result across them identifies that shared path and says nothing about
an ARMv8 core. Two earlier statements to the contrary are now withdrawn: this
section was headed "The cause: QEMU's TCG translation refuses
write-and-execute", and the commit that first recorded the result is titled
"Find the cause: `this core` will not execute a writable page" (`318c9bbb`).
History is left as written; this section supersedes both.

**The fix is to split the window rather than remove the write**: code pages
read-only-executable, data pages writable, which is what every mature OS does.
The experiment above made the whole window read-only, which is why the next
fault is a write to it; the shipped fix needs the two halves mapped separately.



Two further inputs were measured, and both were the last architectural facts
nobody had checked.

**`TCR_EL1 = 0x0000000280193519`** — the register that actually governs the
walk, as opposed to the geometry the bootstrap claimed:

```
T0SZ [5:0]    = 25   -> 39-bit VA, start level 1 for a 4 KiB granule
EPD0 [7]      = 0    -> TTBR0 walks enabled
IRGN0 [9:8]   = 01   -> Normal, write-back, write-allocate, non-transient
ORGN0 [11:10] = 01
SH0 [13:12]   = 11   -> inner shareable
TG0 [15:14]   = 00   -> 4 KiB granule
T1SZ [21:16]  = 25   EPD1 [23] = 0
IPS [34:32]   = 2    -> 40-bit physical addresses
AS [36] = 0   TBI0 [37] = 0
```

So the geometry every read-back assumed is correct: 39-bit, 4 KiB, three levels,
walk starting at level 1, TTBR0 walks enabled. The level-1 index of `0x4009aad4`
is 1 and the level-3 index is 154, as used throughout.

**The leaf was re-read at the fault**, from the handler, by walking from the
`TTBR0` value the handler itself had read, and reaching the tables through the
physmap alias:

```
WALK = 0080000040928003  008000004092a003  000000004009a743
```

That is the same level-1 table descriptor, the same level-2 table descriptor and
the same level-3 leaf as the setup-time read. The tables were not rewritten
between setup and fault, and the walk hardware performs lands on the descriptor
this investigation decoded: valid, page, access flag set, `AttrIndx 0`, `PXN=0`,
`UXN=0`. The architecture's own walk therefore reaches an executable leaf and
still raises a level-3 permission fault.

That closes the last hypothesis with a measurement rather than an inference, and
it leaves the contradiction intact rather than explaining it away.



A level-3 permission fault on an instruction fetch can only come from a small
set of inputs. All four have now been measured at the fault, and all four say the
fetch is permitted:

| input | value | verdict |
| --- | --- | --- |
| translation root | `TTBR0 = 0x5092a000`, firmware root | correct root active |
| descriptor `PXN`/`UXN` | `0x4009a743`: `PXN=0`, `UXN=0` | executable by descriptor |
| `SCTLR_EL1.WXN` | `0xc5183d`, bit 19 clear | not overridden by WXN |
| `MAIR_EL1` at the leaf's `AttrIndx` | `0xff`, so `AttrIndx 0 = 0xff` | Normal write-back, not Device |

`MAIR_EL1 = 0x00000000000000ff` decodes as:

```
AttrIndx 0 = 0xff  Normal, write-back non-transient, RW-allocate
AttrIndx 1 = 0x00  Device-nGnRnE
AttrIndx 2 = 0x00  Device-nGnRnE
AttrIndx 3 = 0x00  Device-nGnRnE
```

The firmware window's leaf uses `AttrIndx 0`, and so does the kernel's own text —
the encoder emits attribute index 0 for every leaf. Both are therefore Normal,
and instruction fetch from Normal memory is permitted. Device memory would have
produced exactly this fault, which is why it was worth re-testing after the
`systemrisc` cfg change; it is not Device.

## The loaded image contains the code the symbols claim

The faulting address was read back from the running guest and compared with the
bootrom image on disk:

```
runtime CODE=f240085fd65f03c0 fa401804fa400844
ELF at 0x4009aad0:  d65f03c0 f240085f fa400844 fa401804
```

They are identical: `ret` at `0x4009aad0`, then `tst x2, #7` at `0x4009aad4`,
which is the entry of `mac_bootloader::volume::disk_info`. So the code is where
the symbol table says it is, `BootInfo::disk_info` points at it, and the loaded
image matches the file byte for byte. A permission fault on a fetch of correct
code at a correct address under an executable leaf remains unexplained.

## Named separately, not explained

`TPIDR_EL1` reads zero at the fault even though `SYSTEM_STACK_READY` reports
`control=0xffffffe010910000` published, and it held a valid pointer when the
vector was installed. This is a separate root cause and is not a consequence of
the mapping work.

Reading the write path makes this much narrower than "unexplained", and it adds
a candidate the framing above does not consider. `crate::machine::cpu::set_hart_control`
(`system/src/machine/aarch64/cpu.rs:105`) has exactly two callers, both on the
primary hart:

* `admit_bootstrap`, `system/src/machine/traps/hart.rs:308` — writes the *cold*
  control pointer, before the registry exists;
* registry publication, `hart.rs:472` — rewrites it to
  `registry.rows[primary_index].control` and then prints `SYSTEM_STACK_READY`
  with that same `control=` value on the next line. The comment above it states
  the intent: "Once REGISTRY exists the old cold pointer is invalid".

So `0xffffffe010910000` is the registry control address, written immediately
before it is printed, and no caller anywhere passes a null value — although
`set_hart_control`'s own safety note permits null, so the type does not forbid
it. A zero reading therefore has only two possible mechanisms:

* **It was never written on the hart that read it.** `TPIDR_EL1` is per-core and
  both writes target the primary only; there is no writer for a secondary hart
  anywhere in `system/`. A secondary reading zero is then not corruption but the
  correct value for a hart that has not published identity. This candidate is
  absent from the framing above, which assumes a single hart — and the framing
  is demonstrably wrong. The `qemu_arm64` launch uses `-smp 4`, and the serial
  log records `CPU_TOPOLOGY enabled=4 primary=0 admission=primary-only`, so
  three secondaries exist and are never admitted. Any fault attributed to one of
  them reads `TPIDR_EL1 == 0` by construction.
* **It was written and then lost** between `hart.rs:472` and the fault — which
  requires an exception return or context restore that carries a stale copy back
  into the register, since nothing writes zero.

Deciding between them is a question of which hart reported the fault, and the
report does not say. That is a smaller question than the one this section
originally posed, and it is answerable by reading the boot order rather than by
another experiment. Neither branch is assumed here.

## Virtio-MMIO enumeration: the record's stride disagreed with the machine

The abort at `0xa004000` (`ESR=0x96000010`, `FSC 0x10`, synchronous external
abort) was first attributed to a probe reaching an unpopulated slot. That
attribution was wrong, and the correction is worth recording because it was
reached by assumption rather than measurement.

The platform record declared `virtio_mmio` with stride `0x1000` and 8 slots.
QEMU 11.1 places **32 transports of `0x200` bytes in a `0x4000` aperture**. The
declared stride was consumed as an address *formula*, so ordinal 4 computed
`0xa000000 + 4 * 0x1000 = 0xa004000` — inside no mapped window at all. The fault
was arithmetic, not an absent device. All three virtio drivers independently
asserted `register_bytes == 4096`, which is the record's stride mistaken for the
machine's window: the same value derived from the same wrong source in four
places, and nothing compared any of them to the tree.

The fix enumerates transports from the device tree and establishes presence from
the **ID register**, not from node presence. That distinction is load-bearing and
contradicts a claim this project made in its own tasking: *every* transport
position in the aperture has a node, attached or not — 32 on arm64, 8 on riscv.
Node presence is not device presence. The claim that a tree lists a node only
for an existing transport is false on both machines and is recorded here so it is
not re-derived.

The regression test is `crates/mac-device-interface/tests/virtio.rs`: it probes a
transport set with an occupied and an unoccupied entry, asserts the empty entry
is reported absent, and asserts only the set's windows are read. Removing the
identity and absence checks makes all three tests fail — it can fail, which is
the requirement for it to count.

**Still latent:** `bootrom-riscv` performs the same arithmetic probe. It is
hidden only because the riscv aperture is fully mapped, exactly as the arm64 one
appeared to be.

**`dma-coherent`, unresolved and now ruled on.** The aarch64 tree states it; the
RISC-V tree states it nowhere; yet the bus reports `COHERENT_DMA` for virtio
transports on both. The ruling is that virtio-MMIO is coherent by the virtio
specification and simple-bus convention — which is what Linux's `virtio_mmio`
assumes, and it sets a coherent mask rather than reading a property — so the
reported value is correct. But it is currently correct *by accident* if it is
inferred from the tree's silence, because silence and assertion are being
treated alike. It must rest on the specification, in a comment that says so.

## Where the boot now stops

After the enumeration change there is **no fault at all** on `qemu_arm64`: the
serial log contains no `ESR=`, no `BOOT_TRAP` and no `SYSTEM_PANIC`. It reaches

```
MMU_READY ... SYSTEM_HEAP_READY ... SYSTEM_STACK_READY control=0xffffffe010914000
BOOT_EMERGENCY_KIT_REJECTED: no framebuffer is available on this platform
```

`raw-hfs-b` still fails, but on the readiness marker timing out rather than on an
exception, which is a different class of failure and an honest one.

The cause is a contradiction, not a crash. `system/src/machine/aarch64/display.rs`
is a stub that reports no framebuffer; `start_services` treats that as fatal;
while the adapter's own documentation says a machine with no display still boots
and reports through serial. One of those two statements is wrong, nothing
compares them, and the disagreement surfaced as a rejected boot. This is the
recurring shape of this project yet again, and it is not worked around.

Worth recording alongside it: `emergency::prepare()` runs `input::prepare_terminal()`
before `display::prepare_terminal()`, so the original `0xa004000` abort was
inside *resident input* enumeration, not the display path it was first guessed
to be.

## Not attempted, deliberately

A typed wrapper that makes a firmware pointer unusable without the switch. The
`cfg` fix makes the invariant hold by construction for every `firmware_call`;
the wrapper is the better end state and belongs after the boot is green.

## Baseline warning

Enabling the `systemrisc` cfg changed 795 branches in shared code on both
architectures. The `qemu_riscv` 19/19 established after that change is a new
baseline: the earlier green said only that those cases never reached the
divergent code. The two numbers are not comparable.
