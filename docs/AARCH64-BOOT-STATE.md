# AArch64 boot state

Where System's AArch64 port stands, what is proven, and what is not. Written to
be read instead of re-derived: the list of things already ruled out is as useful
as the list of things fixed.

## Position

`cargo xtask boot-check --platform qemu_arm64` is held to the same 19-case table
as every platform. It does not currently run: the private SDK cannot build
aarch64 `std`, so the build stops before any case starts (see *What still blocks
`qemu_arm64`*). Built from the cached kernel-side stages instead, `raw-hfs-b`
reaches `EMERGENCY_KIT_READY`, the native service table, `BOOT_FINDER_DOMAIN`
and then refuses Finder's package as stale.

The markers printed, in order:

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
EMERGENCY_KIT_READY services=terminal ...
TRAPS_READY: resident ecall dispatcher ABI=1
SYSTEM_LEASE_PROBE: ok
System: native service table ready
BOOT_FINDER_DOMAIN app=4294967394
FINDER_IMAGE_REFUSED: native ABI layout differs; rebuild System and all native packages
```

AArch64 has never reached `DESKTOP_READY` — or `FINDER_FOLDER_LAYOUT`, the
marker the `raw-hfs-b` case actually requires — in this environment.

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

## The fault that started this: a writable page on a fetch

**Historical — this fault is fixed.** It is kept because the experiments below
were performed against it and because its mechanism is still not fully
explained. The log and the three facts that follow are evidence from the run
that failed, in past tense, not a description of current behaviour. Where the
boot reaches now is in "Where the boot now stops".

`raw-hfs-b` failed with:

```
SYSTEM_TRAP_NO_CONTROL hart=0x80000000 ESR=0x8600000f
  ELR=0x4009aad4 FAR=0x4009aad4
  VBAR=0xffffffd000201800
  TTBR0=0x5092a000 TTBR1=0x40007000
  SCTLR=0xc5183d
```

`ESR 0x8600000f` is an instruction abort taken to EL1: permission fault, level
3. Three facts held together and none of them was wrong:

1. `TTBR0` at the fault was the firmware root, `0x5092a000`, read in the handler
   rather than inferred.
2. The level-3 leaf for the faulting address was executable by descriptor:
   `0x4009a743` — valid, leaf, access flag set, writable, `PXN=0`, `UXN=0`,
   `AttrIndx=0`. The neighbouring page `0x4008ffbc` read `0x4008f743`,
   identical except for the output address, through the same level-2 and
   level-1 tables.
3. `SCTLR_EL1` was `0xc5183d`: MMU, D-cache and I-cache on, `EE` off, `WXN` = 0.
   Nothing in `SCTLR` forced a writable page execute-never.

A level-3 permission fault on a fetch, with the right root, an executable leaf
and `WXN` clear, was not explained by anything readable from inside the guest.
What removed it was splitting the firmware window so the code half is
read-only-executable; what that experiment does and does not establish is in
"Dropping WRITE changed the fault, and what that does not prove" below. The
mechanism is not a settled property of QEMU, and it is not a property of the
core.

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
over. That probe's result is therefore not relied on either way. The fault it
was trying to isolate is gone with the window split; the mechanism is discussed
in the next section.

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
image matches the file byte for byte. So code identity is excluded: the fetch
was of correct code at the correct address. What remained was the permission —
that leaf was writable as well as executable — and making it read-only is what
removed the fault. The window split is how it became read-only.

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
  them reads `TPIDR_EL1 == 0` by construction. That is topology, not a failure:
  the resident scheduler is single-hart by design and admits the primary only,
  with multi-hart dispatch belonging to the multi-scheduler extension.
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

The record was corrected to the machine's own numbers — stride `0x200`, 32 slots
— and those two fields are not decorative: `tools/mac-build/src/platform/memory/io.rs`
multiplies them to size the emitted `BOOT_MMIO_REGIONS` window, so the old pair
mapped `0x8000` over a `0x4000` aperture. The corrected record maps
`0xa000000..0xa004000`, exactly the aperture. It was confirmed by asking the
machine rather than the documentation:
`qemu-system-aarch64 -machine virt,dumpdtb=virt-a64.dtb -cpu max -m 2G -smp 4`
(QEMU 11.1.0), then reading every `virtio,mmio` node's `reg` out of the dump —
32 nodes, `0xa000000` + `n * 0x200`, each `0x200` bytes, ending at `0xa004000`.
The same dump for `qemu-system-riscv64` gives 8 nodes of `0x1000` at
`0x10001000..0x10009000`, which is what `riscv_base.json` already stated, so that
record needed no change.

The fix enumerates transports from the device tree and establishes presence from
the **ID register**, not from node presence. That distinction is load-bearing and
contradicts a claim this project made in its own tasking: *every* transport
position in the aperture has a node, attached or not — 32 on arm64, 8 on riscv.
Node presence is not device presence. The claim that a tree lists a node only
for an existing transport is false on both machines and is recorded here so it is
not re-derived.

A transport is a bus, and the device on it is identified by the ID register, so
the transport node is no longer enumerated a second time as a board peripheral.
Both views are filtered from one classification in
`system/src/machine/platform_devices.rs`, and admission checks on the machine's
own tree that no transport node reaches the peripheral view. Transports stay
covered for pool exclusion and mapping by the record's declared bootstrap
aperture, which the paragraph above now states correctly.

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

**An interrupt the tree does not attribute is now absence, not zero.** The
device description carried `interrupt: u32`, and a node whose `interrupt-parent`
could not be resolved was published as interrupt 0 — on both machines
`interrupt_base` is 1, so that was a number no controller decodes, and a driver
could not tell it from "raises none". `Description.interrupt` is now
`Option<u32>`: `None` means the machine states no interrupt for the device, and a
driver that needs one refuses the device rather than vectoring a number that was
never stated. The wire carries the fact in `INTERRUPT_STATED`, and `decode`
refuses a word that disagrees with the flag, so absence cannot be re-encoded as a
sentinel. A node that states interrupts the kernel cannot attribute is refused at
admission by its compatible string, which is also why the `Missing` and
`NoInterruptParent` refusals exist as machine facts rather than defaults.

## The display adapter: the stub, not the machine, said there was no framebuffer

`system/src/machine/aarch64/display.rs` was a stub whose every entry point
reported that no display existed, so `start_services` refused the boot with
`BOOT_EMERGENCY_KIT_REJECTED: no framebuffer is available on this platform`.
The record disagrees: `platforms/qemu_arm64.json` declares `virtio-gpu` with a
`ramfb` framebuffer, the generated launch attaches both, and the install set
packages the `Virtio Gpu` module. The machine has a display; the adapter could
not find one.

The display path was already shared and only the aarch64 binding was missing.
`crates/mac-boot-screen` holds the one audited fw_cfg RAMFB binding, but every
item in it was gated `#[cfg(target_arch = "riscv64")]` — the shared mechanism
compiled for exactly one architecture. The RISC-V adapter that drives it,
`system/src/machine/riscv64/display.rs`, was also machine-neutral apart from two
instructions (`rdtime` for the tick source and a `fence iorw, iorw` before
publishing a frame), both of which already exist in the selected architecture's
`cpu` adapter.

So the port is a deletion and a move, not a second implementation:

- the `riscv64` cfg gates are gone from the RAMFB binding, and the one
  architecture-specific instruction in it — the barrier before the device
  doorbell — is selected in one `barrier()` function with a host fallback,
  matching `crates/mac-driver/src/arch.rs`;
- the adapter moved to `system/src/machine/display.rs`, a machine-neutral module
  above the selection point, using `cpu::ticks()` and `cpu::barrier()`;
- both architecture `display.rs` files are deleted, including the stub.

Two things were wrong beyond the stub. The record did not declare the machine's
fw_cfg window, so the generated `FW_CFG_BASE` was `0`; the window is
`0x09020000`+`0x18`, confirmed from the tree QEMU 11.1 supplies for
`-machine virt` (`fw-cfg@9020000`, compatible `qemu,fw-cfg-mmio`), and is now
declared as `memory.fw_cfg`. It does not overlap the declared UART or virtio
windows, so validation accepts it, and the page it needs was already being mapped
identity from the tree's node; declaring it also excludes it from the heap.

Evidence: `qemu_arm64` now prints `EMERGENCY_KIT_READY` with the same
`frame_bytes=1920000` as `qemu_riscv`, which means the RAMFB binding reached the
real fw_cfg window, matched its signature and DMA interface, found `etc/ramfb` at
28 bytes, and translated both DMA buffers — the first time this path ran on
arm64. The scanout is *not* configured: that is `setup_framebuffer()`, which runs
later, and the boot faults before it. The display is prepared and unproven, not
working. `qemu_riscv` still passes 19/19 and still prints
`RAMFB 800x600 addr=0x88000000` followed by `DESKTOP_READY`.

## The signalling target is a type, not an integer

After the display fix the boot faulted in
`machine::monitor::PublicationLease::finish`, reached from
`paging::modules::Mapping::publish` → `monitor::publish_instructions()`:

```
SYSTEM_TRAP_NO_CONTROL hart=0000000080000000 ESR=0000000096000046 ELR=ffffffd00034bda4 FAR=0000000000000002
```

`ESR` decoded as a data abort at the same EL writing to VA `2`; `FAR=2` was
`notify()`'s `ptr::write_volatile(entry.signal as *mut u32, 1)`, and `2` is
`bootrom-aarch64-virt`'s `signal(hart) = 1 << hart` for hart 1.

The ABI already stated the two meanings and forbade exactly this: on a
`Signalling::Cells` platform an entry's target is the address of that hart's
CLINT `msip` cell, and on a `Signalling::Controller` platform it is a mask the
controller decodes. The kernel's `notify()` implemented only the first meaning
and never consulted the published `Signalling` at all. The admission check
compared `Control.signal` with `Entry.signal`, which are the same value written
twice, so it could not detect the disagreement — the same shape as the virtio
stride: one value, two meanings, no comparison that could fail.

It was not only a wrong address. The parked harts are `wfe` loops in the bootrom
with no vector table installed, so they could not acknowledge a controller
interrupt even if one had been sent. The aarch64 monitor transport did not exist
on either side; the kernel's publication loop assumed the RISC-V one.

### What replaced it

`mac_abi::boot::monitor::SignalTarget` is the published target: a
`#[repr(transparent)]` wrapper with no integer or pointer conversion. It can only
be used through `Signalling::route`, which returns `Routed::Cell(CellAddress)` or
`Routed::Controller(ControllerTarget)` according to the kind the loader declared.
Only `CellAddress` has `address()`; `ControllerTarget` names no address at all.
`Control::signal` and `Entry::signal` are `SignalTarget`, so the old
`entry.signal as *mut u32` is now a compile error rather than a convention. A
standalone crate depending on the real `mac-abi` was compiled with the old
`notify` body and with a controller target used as an address; the compiler said:

```
error[E0605]: non-primitive cast: `SignalTarget` as `*mut u32`
  --> src\main.rs:9:29
   |
 9 |         ptr::write_volatile(entry.signal as *mut u32, 1);
   |                             ^^^^^^^^^^^^^^^^^^^^^^^^ an `as` expression can only be used to convert between primitive types or to coerce to a specific trait object

error[E0599]: no method named `address` found for struct `ControllerTarget` in the current scope
  --> src\main.rs:26:44
   |
26 |                 ptr::write_volatile(target.address() as *mut u32, 1);
   |                                            ^^^^^^^ method not found in `ControllerTarget`
```

The first is the exact expression that faulted at VA `2`. The second is why a
controller target cannot take its place: it has no address at all, so the only
way to reach a writable value is a cell route.

`notify()` now takes the admitted transport and matches on its route. A cell
route writes the cell; a controller route returns `Error::Signalling` and prints
`MONITOR_SIGNALLING_REFUSED hart=N target=0xM` naming the reason: the target is a
controller mask, not an address, and cross-hart dispatch belongs to the
multi-scheduler extension. Nothing in the resident path writes a target.

The bootroms were converted with it. `bootrom-riscv` states its kind once
(`ROUTE`) and writes cells only through `ROUTE.route(entry.signal)`;
`bootrom-aarch64-virt` builds `SignalTarget::new(1 << hart)`, which cannot be
written to because it is not an address.

### The resident path owns no peer it cannot signal

The resident scheduler schedules one hart. That is its whole scope, and it is not
a membership list a platform record could enlarge: multi-hart dispatch is the
multi-scheduler extension's (`extensions/multi-scheduler`, `SCTL`/`MLIF`,
`TAKEOVER | MULTI_HART | PARK`), behind its own takeover seam, and hart count is
topology rather than dispatch authority.

So the resident monitor's signalable peer set is derived from the route it can
actually write, not from how many CPUs the machine has: `Admitted::peers` yields
the non-primary rows on a cell bank and nothing at all through a controller. The
broadcast paths (`PublicationLease::finish`, `stop_peers`) and CPU initialization
all iterate that set, so on a controller platform the boot publishes locally,
starts no peer, and schedules on the primary. On a cell platform the behaviour is
unchanged: peers are started, published to and stopped exactly as before.

This is why the aarch64 boot no longer dies in `publish_instructions`. It is not
a workaround for the cast — the cast cannot be written any more — and it does not
leave the fault waiting for a second hart: a second hart now reaches a refusal
that names the extension.

### Not implemented, deliberately

Controller signalling in the resident path. It cannot own harts it does not
schedule, and GIC SGI delivery with peer vectors and an acknowledgement handler
belongs behind the extension's takeover seam. The resident path refuses by name
until then, and the type is what stops the extension from inheriting the old cast
when it takes over.

## Ordering in `emergency::prepare()`

`input::prepare_terminal()` runs before `display::prepare_terminal()`. Neither
depends on the other: input claims and pre-builds its dedicated virtio rings, and
display performs fw_cfg discovery and reserves its terminal DMA, and each refuses
on its own terms. The order is therefore not load-bearing for correctness, and
nothing enforces it. The one argument for keeping it is failure order: input
enumeration is the step that can refuse on machine facts (it requires exactly the
recorded device count), and running it first refuses the boot before the
terminal's claim on the fw_cfg device has been published. That claim does not
block normal configuration — `configure()` may still run against a prepared
device — so this is a preference about which refusal is reported first, not a
requirement, and it is worth knowing that a display failure can only be reported
after input has already succeeded.

## Where the boot now stops

The emergency kit is accepted and the boot reaches `BOOT_FINDER_DOMAIN`. It used
to fault there in monitor instruction publication, at VA `2`; that fault is gone.
It then faulted on the first dynamically mapped definition, and that fault is
gone too (see the next section). The boot now refuses Finder's package:

```
BOOT_FINDER_DOMAIN app=4294967394
FINDER_IMAGE_REFUSED: native ABI layout differs; rebuild System and all native packages
```

That refusal is `Descriptor::decode` (`crates/mac-code-resource/src/lib.rs:67`)
rejecting the package's `NATIVE_LAYOUT_FINGERPRINT`. It is correct: the aarch64
application packages in the build predate a change to `SystemServices` and must
be rebuilt. It is also **not** the fault this section used to describe, and it is
not a workaround target — the check stays.

Nothing was done to make a marker appear. The fatal `start_services` path is
unchanged: a missing framebuffer on a machine whose record declares one is still
a refused boot, and `BOOT_EMERGENCY_KIT_REJECTED` is still fatal. The boot passes
that check because a framebuffer is now found, not because the check was relaxed.

## The fetch at `0x430fd000` was a table index, not a placement

The old report read this fault as the application: `0x3000` below
`virtual_memory.application_relocation_base`. That reading was wrong in a way
worth recording, because two numbers lined up and neither of them was the
application.

**It is the Progress Bar CDEF, and its placement and its entry agree exactly.**
Both state `0x430fd000`:

* `system/src/toolbox/resources/definitions.rs:732` places every definition
  top-down: `base = DEFINITION_LIMIT - extent`. The CDEF's linked span is
  `0x3000` (`CDEF_PROGRESS_BAR`: entry `0x42100000`, `.rodata` at `+0x2000`, end
  `0x42020af`), so `base = 0x43100000 - 0x3000 = 0x430fd000`. For this record
  `DEFINITION_LIMIT` and `application_relocation_base` are the same number,
  `0x43100000`, which is what made the two look like one window.
* `system/src/machine/paging/modules.rs:1011` records `plan.entry()`, which is
  `base + (descriptor.entry - descriptor.load_base)` — `0x430fd000`.

What disagreed was the table the mapping was written into.
`system/src/machine/aarch64/page_layout.rs::index` indexed `leaf_sizes()` with a
level counted from the finest leaf, and this format writes that list
largest-first — `[1 GiB, 2 MiB, 4 KiB]`. So `index(addr, 2)` returned the 4 KiB
index and `index(addr, 0)` returned the 1 GiB index: the root slot was written at
entry `253` and the leaves at index `1` of the level-3 table, while the walk reads
root entry `1`. Every page of the definition collided into one slot.

Measured in the guest before the fix, by reading the kernel root through the
QEMU monitor (`xp /512gx 0x40007000`, TTBR0 taken from the trap report):

```
root[1]   = 0            <- what the walk reads
root[253] = 0x40917003   <- where the level-2 branch was written
0x40918000[1] = 0x00600000409167c3   <- the only leaf, at the 1 GiB index
```

`0x86000005` decodes as `EC=0x21`, which is an instruction abort taken *without a
change of exception level* (Linux's `ESR_ELx_EC_IABT_CUR`); `-d int` agrees —
`Taking exception 3 [Prefetch Abort] on CPU 0 ...from EL1 to EL1`. Earlier
readings of this syndrome as "from a lower exception level" were the inverted
EC table, not a different fault.

**The fix is one statement of level to leaf size.** `index` now asks the shared
geometry for the leaf, `PageLayout::leaf_at(level)`, which counts from the finest
leaf and is already what `MappingPlan`, `pages/mapping.rs` and `pages/walk.rs`
use. RISC-V's adapter (`Sv39::index`) always counted from the finest leaf, which
is why `qemu_riscv` was green while `qemu_arm64` was not: the two adapters
disagreed about what `level` means, and nothing compared them.

The index arithmetic, computed with the real crates on the host for
`0x430fd000`:

```
level 0: leaf_sizes()[0]=0x40000000 index=0x1     leaf_at(0)=0x1000   index=0xfd
level 1: leaf_sizes()[1]=0x200000   index=0x18    leaf_at(1)=0x200000 index=0x18
level 2: leaf_sizes()[2]=0x1000     index=0xfd    leaf_at(2)=0x40000000 index=0x1
```

**What this does not establish.** No aarch64 definition has yet been *executed*
after the fix, because the boot now stops earlier, on Finder's stale package.
The mapping is the only thing changed, and the fault it removes is the only
evidence for it; the definition path still needs a boot that gets past Finder
admission to be called proven.

## What still blocks `qemu_arm64`

Three defects stand between this boot and `FINDER_FOLDER_LAYOUT`, none of them in
the mapping code. They were found while trying to run the case table and are
recorded with their exact sites.

1. **`mac-system` did not compile for aarch64 at all.** `Registry::execution_handlers`
   was `#[cfg(target_arch = "riscv64")]` (`system/src/drivers/registry.rs:1191`)
   while its caller, `system/src/drivers/runtime.rs:986`, is shared code. The
   method is architecture-neutral and its empty result is already the documented
   "no execution provider" case (`system/src/toolbox/processes/execution.rs:13`),
   so the gate was deleted rather than the caller. This is the same defect shape
   as the RAMFB binding: an architecture gate stranding shared code.
2. **The private SDK cannot build aarch64 `std`.**
   `platforms/rust/library/std/src/sys/pal/systemrisc/native_tls.rs:11` selects a
   machine thread-pointer adapter only for `riscv64` and emits
   `compile_error!` otherwise. `sdk::prerequisites` builds `std` for the
   platform's `std_target`, so `cargo xtask boot-check --platform qemu_arm64`
   stops there before it builds anything, and the application packages cannot be
   regenerated against the current fingerprint. An aarch64 adapter is
   `TPIDR_EL0`, not a general register.
3. **The aarch64 trap frame does not carry `TPIDR_EL0` or `x30`.**
   `TrapFrame::registers` is `x0..x29` and `system/src/machine/aarch64/trap_entry.S`
   neither saves nor restores `x30`; `TPIDR_EL0` is not touched anywhere in
   `system/`. RISC-V's user thread pointer is `tp`, which is `registers[4]`, so it
   survives a trap for free — the aarch64 port has no equivalent. Saving
   `TPIDR_EL0` is a prerequisite for `std` TLS, and losing `x30` across a trap
   breaks any preempted user function, so both belong to one thread-context
   change rather than to a TLS-only patch. The floating-point/NEON state is in
   the same position, and user code is built with `+neon`.

## Not attempted, deliberately

An aarch64 thread context. Saving `TPIDR_EL0`, `x30` and the floating-point/NEON
state across a trap is one change with one owner — the thread/std boundary — and
it is a prerequisite for `std` TLS and for preempting user code at all, not for
the mapping fix. It is listed under *What still blocks `qemu_arm64`* rather than
started here, because a TLS-only patch would compile while leaving the frame
lossy and would make the next reader believe the context was complete.

A typed wrapper that makes a firmware pointer unusable without the switch. The
`cfg` fix makes the invariant hold by construction for every `firmware_call`;
the wrapper is the better end state and belongs after the boot is green.

GIC SGI delivery for the aarch64 monitor route, for the reasons in its own
section: the resident scheduler is single-hart by design and multi-hart
dispatch belongs to the multi-scheduler extension behind its takeover seam,
and the controller path refuses by name until the parked harts have vectors
and a handler.

## Baseline warning

Enabling the `systemrisc` cfg changed 795 branches in shared code on both
architectures. The `qemu_riscv` 19/19 established after that change is a new
baseline: the earlier green said only that those cases never reached the
divergent code. The two numbers are not comparable.
