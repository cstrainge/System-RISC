# AArch64 boot state

Where System's AArch64 port stands, what is proven, and what is not. Written to
be read instead of re-derived: the list of things already ruled out is as useful
as the list of things fixed.

## Position

### Position after the probe fixtures were ported to `std` (2026-11-08)

The two probe fixtures now build and run on aarch64, so the gate constructs
every case instead of stopping while it assembles the table. `fonts-probe`
passes. `pipes-probe` is the first case that fails, and it fails on a real guest
fault rather than on a missing artifact — see *The pipes probe faults after its
first child thread completes*.

Measured with `cargo xtask boot-check --platform qemu_arm64` at `debb17ae`:

| case | result |
| --- | --- |
| `no-disk` | passed |
| `raw-hfs-b` | passed — `FINDER_FOLDER_LAYOUT window=` and `DESKTOP_READY:` |
| `fonts-probe` | passed — `FONTS_PROBE_CATALOG families=23`, `FONTS_PROBE_STYLE_OK`, `FONTS_PROBE_PASS cases=5` |
| `pipes-probe` | **failed**: the second case, five markers, then a user fault on the woken thread; see below |

So the arm64 case count is 3 of 19 passing, and the first remaining failure is
`pipes-probe`. Before the port the gate reported 2 of 19 and exited while
building `fonts-probe` for `aarch64-unknown-none`.

The same tree on `qemu_riscv` passes **19 of 19**, with `RAMFB 800x600`,
`FINDER_FOLDER_LAYOUT window=` and `DESKTOP_READY:` in `raw-hfs-b` and
`PIPES_PROBE_PASS cases=11` / `DESKTOP_READY: pipes probe pass` in
`pipes-probe`. The ported fixtures are the same source on both platforms.

### Position after the `x4` fix (2026-11-08)

The intermittent level-3 permission fault on a read of the per-hart control
block is fixed. The cause was the aarch64 exception return: it restored the
interrupted `x4` and then used `x4` to clear the entering flag, so **every**
resumed context — application or System — carried the control block's address in
the fifth argument register. See *Fixed: the exception return handed every
resumed context the control block in `x4`* for the trace.

Measured on `9f5fd6c9`+ the fix, artifact `dist-arm64-el0` built from
`0995b36c`, eleven `cargo xtask boot-check --platform qemu_arm64` invocations:

| case | result |
| --- | --- |
| `no-disk` | passed, 11 of 11 |
| `raw-hfs-b` | passed, 11 of 11 — `FINDER_FOLDER_LAYOUT window=` and `DESKTOP_READY:` in every serial log, no `CRASH_REPORT_READY` in any of them |
| `fonts-probe` and later | **not constructed** in that run. `boot-check` builds the probe fixtures while it assembles the case table, and those fixtures could not then be built for aarch64. That limit is gone: see *The probe fixtures are `std` applications now* |

So the arm64 case count is 2 of 19: the two cases that do not need a probe
fixture. The gate exits 1 on the fixture build, not on a guest failure, and the
desktop case is no longer intermittent.

The comparison is the record from `af8d78d7`: of seven arm64 runs, five printed
`FINDER_FOLDER_LAYOUT` and four printed `DESKTOP_READY`, and a run that printed
neither had taken the control-block read. Both markers now appear in every run,
with no crash report to recover from.

The same commit on `qemu_riscv` passes 19 of 19 cases, with `RAMFB 800x600`,
`FINDER_FOLDER_LAYOUT window=` and `DESKTOP_READY:` in `raw-hfs-b` and no
`CRASH_REPORT_READY` in that log. The fix is in the AArch64 entry only; the
RISC-V entry already reloaded its scratch.

### Position after the application set, the tick and the power findings (2026-11-08)

Three defects reported from an interactive desktop session are settled here.

- **Applications.** `platforms/qemu_arm64.json` installed three applications
  because aarch64 `std` did not build. It now installs the same eleven as
  `qemu_riscv`, with the same `Applications/Utility` and
  `Applications/Utility/Cli` exceptions, and all eleven package. Shelly exposed
  a real defect in the aarch64 GOT decoder, described below.
- **Double click.** Fixed. The classic tick was 6.25 times too fast, so the
  half-second window was 80 ms of real time and no folder on the boot volume
  could be opened.
- **Shut Down and Restart.** Still disabled, and correctly so: this machine has
  no finisher register to declare.

Measured after all three, artifact `dist/qemu_arm64`:

| case | result |
| --- | --- |
| `no-disk` | passed |
| `raw-hfs-b` | passed — `RAMFB 800x600`, `FINDER_FOLDER_OPEN`, `DESKTOP_READY:` |
| `fonts-probe` and later | not constructed, as before |

`qemu_riscv` passes 19 of 19 ROM boot checks, with `RAMFB 800x600`,
`FINDER_FOLDER_OPEN` and `DESKTOP_READY:` in the `raw-hfs-b` log and no crash
report in it.

### How it stood when the physmap read was unexplained

`cargo xtask boot-check --platform qemu_arm64` builds its private SDK and enters
the case table. `raw-hfs-b` reaches the desktop; this is one of the observed
runs, and the intermittency described below is part of the state:

```
INTERRUPTS_READY: controller=gic-v2 distributor=0x8000000 cpu_interface=0x8010000 timer=30 security=0 lines=9 ctlr=0x1 group=0x0 enabled=0x4000ffff prio=0xa00000 target=0x1010101 gicc_ctlr=0x1 pmr=0xf0
RAMFB 800x600 addr=0x50930000
SYSTEM_CPUS_READY primary=0 admitted_peers=0 dispatch=primary-only
TIMER_ARMED: requested=0x196c99c2 cval=0x196c99c2 ctl=0x1 count=0x196c79a7 freq=0x3b9aca0
FONTS_PUBLISHED revision=1 families=23 volume=66113 directory=21
DISPLAY_MODE_CHANGED 1280x800
LAUNCH_DIRECTORY_READY owner=4294967394 volume=66113 node=20 policy=Documents
SYSTEM_FINDER_LAUNCH entry=0x400000
BOOT_PROGRESS completed=7 total=7
PROCESS_REGISTERED app=4294967394 name=Finder
Finder: native application entry reached
FINDER_POWER_CAPABILITIES shutdown=false restart=false
CRASH_REPORT_READY app=4294967394 window=4294967543 bytes=1013 lines=40 page_lines=22 flags=0
NATIVE_EXIT_RECOVERY_QUEUED app=4294967394 kind=abort code=255 thread=4294967479 state=owned-finder-flight
FINDER_RECOVERED old=4294967394 new=4294967550 thread=4294967601 state=runnable
Finder: native application entry reached
FINDER_POWER_CAPABILITIES shutdown=false restart=false
FINDER_VOLUME id=66113 name=System RISC
FINDER_FOLDER_OPEN volume=66113 cnid=2 window=4294967664 name=System RISC
FINDER_FOLDER_LAYOUT window=4294967664 volume=66113 folder=2 first=16,28,48,60 frame=8,46,412,234 count=3
FINDER_FOLDER_READY window=4294967664
DESKTOP_READY: Finder and native Window Manager
```

The data abort on Finder's own stack is gone. Before the page-protection fix it
was a permission fault on a write, `ESR 0x9200004f`, `FAR 0xfa8fe0`, from EL0;
that fault no longer occurs at all.

What remains is a different fault, and it is intermittent. With supervisor pages
genuinely unreachable from EL0, Finder sometimes takes a level-3 permission
fault on a **read** of a physmap address:

```
Taking exception 4 [Data Abort] on CPU 0
...from EL0 to EL1
...with ESR 0x24/0x9200000d      <- data abort from lower EL, permission fault, level 3, READ
...with FAR 0xffffffe010914002
...with SPSR 0x60000000          <- EL0t
...with ELR 0x43c600             <- Browser::draw_items+0x284
```

A second instance of the same fault in that run is `FAR 0xffffffe010914040`,
`ELR 0x43a97c` (`Browser::desktop::Browser::change_desktop_selection+0x124`); a
third, captured in the crash report of another run, is `ELR 0x42d270`
(`Startup::advance+0x7b0`). `0xffffffe010914000` is the per-hart monitor control
block — `SYSTEM_STACK_READY` prints it as `control=0xffffffe010914000` — so an
application is dereferencing a kernel-half pointer into it, at different offsets
and from different call sites. The inverted `AP[1]` had been letting that read
succeed.

That is the finding, and it is an application/ABI defect rather than a
protection one: the page tables are doing exactly what they now say. Its
intermittency is why the boot sometimes completes — of seven arm64 runs made
while establishing this, five printed `FINDER_FOLDER_LAYOUT window=` and four
printed `DESKTOP_READY`. The failing instance is replaced by the System, and the
replacement does not always repeat the access.

The gate does not report green on this platform for two reasons, neither of them
Finder's page protection:

- `raw-hfs-b` asserts `BOOT: entering System`, the RISC-V bootstrap's wording,
  and the AArch64 bootstrap used to say `BOOT_AARCH64_ENTERING`. The words are
  now shared (`mac_bootloader::report::ENTERING_SYSTEM`), so the case can judge
  the boot instead of the spelling.
- The case's readiness marker is `DESKTOP_READY:` with a 60 s budget, and an
  arm64 run that hits the physmap read above twice never prints it.

`qemu_riscv` passes 19/19 and is the gate that matters for landing.

**Evidence that System now runs at EL1.** With `-d int` on the same image, the
saved status of every privileged trap is `M[3:0] = 0b0101` — the idle loop's
timer interrupts arrive with `SPSR 0x5` (EL1h, DAIF unmasked), and the resident
gateway's own calls with `SPSR 0x…3c5` (EL1h, DAIF masked) — while application
calls arrive with `SPSR 0x60000000` and `SPSR 0x0`, both `M[3:0] = 0b0000`
(EL0t). Before this change every one of those frames reported `M[3:0] = 0`.


## The arm64 machine has no poweroff register, so Shut Down and Restart stay disabled

Finder prints `FINDER_POWER_CAPABILITIES shutdown=false restart=false` and
disables both Special-menu items because no `POWR` provider is live. That is the
correct answer for this record, and it is not a record that forgot a field: this
machine has no device to declare.

`platforms/qemu_riscv.json` installs `qemu-power-device`, and
`platforms/riscv_base.json` declares both the MMIO window that driver claims
(`mmio_devices`, `test@100000`, compatible `sifive,test1`) and the
`fatal_shutdown` block the terminal path writes. Both describe QEMU's
`hw/misc/sifive_test.c`, which the RISC-V `virt` machine creates and the AArch64
`virt` machine does not.

Measured on the installed QEMU 11.1.0:

| check | RISC-V `virt` | AArch64 `virt` |
| --- | --- | --- |
| `-machine virt,dumpdtb=` device tree | `test@100000`, `compatible = "sifive,test1"` | no such node |
| `-device sifive_test` | accepted | `'sifive_test' is not a valid device model name` |

The address and the two values the RISC-V record holds therefore have no
counterpart to copy. Declaring one anyway would name a register the machine does
not answer, and the menu items would be enabled and do nothing — the failure the
disabled state avoids. Records describe machines; this is a machine without that
device.

AArch64 `virt` powers off the way the architecture does: PSCI. The machine's own
tree states it, `psci { method = "hvc"; }`, and the guest already runs at EL1
with QEMU's implementation above it at EL2. `SYSTEM_OFF` is function
`0x84000008`, `SYSTEM_RESET` `0x84000009`, both called with `hvc #0`.

That is a different mechanism from a finisher register, and enabling the items
needs it to exist rather than a record correction:

- `fatal_shutdown` is the panic-time terminal signal, not the ordinary power
  path. Even on RISC-V the menu is enabled by the external `POWR` module that
  claims the declared window, not by `fatal_shutdown`.
- A PSCI provider needs the record to state which conduit the machine offers and
  the build to emit it, plus a module that offers `POWR` without claiming a
  device. A match-free module is admitted — `drivers/stuffit-filesystem` is one —
  so the module side is possible, but the record and build side does not exist.
- `drivers/qemu-power-device` cannot serve it as written: its `build.rs` builds
  only for `riscv64imac-unknown-none-elf`, and it matches `sifive,test1`.

None of that is implemented. Until it is, the disabled items are the honest
answer and this section is the reason, so the next reader does not re-derive it
or reach for an address that is not there.

Shutdown ordering is not the obstacle. `system/src/toolbox/power/mod.rs`
refuses unfinished file transactions, requires other applications to be closed
and flushes every writable block device before it calls the provider, so a PSCI
provider would inherit that ordering rather than bypass it. The hazard would be
a power path that skipped those gates, not PSCI itself.

## Fixed: the classic tick ran 6.25 times too fast, so a double click never fit

Symptom: double-clicking did nothing, so a folder on the boot volume could not be
opened, while single clicks and selection worked.

Finder decides a double click at `apps/finder/src/browser.rs:2461-2472`: two
presses on the same item, with
`event.input().when.wrapping_sub(time) < 30`. Those are classic ticks, and the
Toolbox reports them at 60 Hz, so the window is meant to be half a second.
`when` comes from `Toolbox::uptime()` (`system/src/toolbox/runtime.rs:670`),
which read the machine counter and divided it by a hardcoded `10_000_000`.

That divisor is the RISC-V `virt` timebase. The AArch64 counter runs at 62.5 MHz,
which the guest prints for itself:

```
BOOT_AARCH64_COUNTER: hz=0000000003b9aca0
TIMER_ARMED: requested=0x146b3bb4 cval=0x146b3bb4 ctl=0x1 count=0x146b1efe freq=0x3b9aca0
```

So `uptime()` advanced 375 per second instead of 60 and every interval measured
in it was 6.25 times shorter: the 30-tick window became 80 ms of real time. A
first click in Finder also costs the guest tens of milliseconds of work, so two
presses 20 ms apart already arrived more than 30 ticks apart and the pair was
never recognised. This is why the desktop could be used but nothing could be
opened.

`machine::monitor::ticks_to_event_ticks` now converts against the admitted
counter rate, the inverse of the `event_ticks_to_ticks` that already existed for
the other direction and already carried the comment "avoiding a machine-specific
10 MHz constant". On RISC-V the arithmetic is identical — the old expression was
`floor(a*60/10^7)` by quotient and remainder — so that platform's behaviour does
not change.

Measured on the shipped input path: `input-send-event` to the record's
`virtio-tablet-device`, two left clicks at one point on the boot volume's first
item, serial read for `FINDER_CLICK_HANDLED` (both presses arrived in every run)
and for a new `FINDER_FOLDER_OPEN ... name=Applications`.

| artifact | gap between presses | `FINDER_FOLDER_OPEN` | opened |
| --- | --- | --- | --- |
| `qemu_arm64` before the fix | 5 ms | 1 → 2 | yes |
| `qemu_arm64` before the fix | 20 ms | 1 → 1 | no |
| `qemu_arm64` before the fix | 50 ms | 1 → 1 | no |
| `qemu_arm64` before the fix | 100 ms | 1 → 1 | no |
| `qemu_arm64` before the fix | 200 ms | 1 → 1 | no |
| `qemu_arm64` after the fix | 200 ms | 1 → 2 | yes |
| `qemu_arm64` after the fix | 400 ms | 1 → 2 | yes |
| `qemu_arm64` after the fix | 700 ms | 1 → 1 | no |
| `qemu_riscv` after the fix | 200 ms | 1 → 2 | yes |

The fixed window ends between 400 ms and 700 ms, which is the 500 ms the constant
asks for; the unfixed one ended below 20 ms. Nothing was widened: the constant is
still 30, and the change is that a tick is now a sixtieth of a second on this
machine as well.

One instance of the same assumption is left alone here because it is a timeout
rather than an interval: `crates/mac-bootloader/src/virtio_blk.rs:348` treats
`elapsed > 10_000_000` counter ticks as one second, which on AArch64 is 160 ms.
It has not been observed to matter, and it is a separate change.

## Fixed: the aarch64 GOT pairing could not tell two sites apart

Packaging Shelly — the largest application — failed with `GOT relocation does not
identify a matching ADRP/LDR register pair`. The message named the check, not the
cause, so the check was made to print the pair it refused:

```
GOT-DIAG load=0x40021c target=0x40021c page=0x525e1c ldr_rn=10 adrp_rd=9 paired=false pages=3
```

The load read through `x10` and the site the search returned used `x9`, with the
same target. Three ADRP sites existed and two of them named that one cell, which
is ordinary: a GOT cell is created once and referenced from everywhere. Two
things were wrong in `crates/mac-formats/src/elf/relocation/aarch64.rs`:

- the sites were sorted by address and then searched by target, so binary search
  walked an order the slice did not have and could answer "absent" for a site
  that was present;
- a search over equal targets returns an arbitrary one of them, and the register
  was the only thing that could have told them apart.

The site is now chosen by the register that pairs it with the load, preferring
one no load has claimed, which is what makes the closing "every site is paired"
check mean something. `cargo xtask --platform qemu_arm64 stage apps` packaged nine
applications before the change and stopped at Shelly; it packages all eleven
after.

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
gone too (see the next section). It then refused Finder's package as stale:

```
BOOT_FINDER_DOMAIN app=4294967394
FINDER_IMAGE_REFUSED: native ABI layout differs; rebuild System and all native packages
```

That refusal is `Descriptor::decode` (`crates/mac-code-resource/src/lib.rs:67`)
rejecting the package's `NATIVE_LAYOUT_FINGERPRINT`. It was correct: the aarch64
application packages in the build predated a change to `SystemServices` and had
to be rebuilt. It is also **not** the fault this section used to describe, and it
was not a workaround target — the check stays.

That refusal is now history. With `std` building for aarch64, `boot-check`
regenerates the packages, Finder passes the fingerprint check, `FONTS_READY`
prints, and the boot paints the Progress Bar, loads its drivers and reaches
`SYSTEM_CPUS_READY`. It then waited for an interrupt source that did not exist;
that is fixed (see *What still blocks `qemu_arm64`*), and the boot now runs the
scheduler to `SYSTEM_FINDER_LAUNCH`. It stops in Finder, for the reason in
*The next blocker*.

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

### Fixed: the private SDK could not build aarch64 `std`

`platforms/rust/library/std/src/sys/pal/systemrisc/native_tls.rs` selected a
machine thread-pointer adapter only for `riscv64` and emitted `compile_error!`
otherwise, so `sdk::prerequisites` — which builds `std` for the platform's
`std_target` — stopped the whole `boot-check` before any case ran, and the
application packages could not be regenerated against the current fingerprint.

The adapter is now selected once at that module boundary:
`native_tls/riscv64.rs` reads `tp`, `native_tls/aarch64.rs` reads and writes
`TPIDR_EL0`. That register is what AArch64 provides for a software per-thread
pointer and what Linux uses; it is per-thread, not a global. System's own control
block stays in `TPIDR_EL1`.

### Fixed: the trap frame did not carry `TPIDR_EL0` or `x30`

`TrapFrame::registers` is 30 slots. That covers RISC-V's `ra` (`registers[0]`)
and `tp` (`registers[2]`), which is why RISC-V's thread pointer and return link
survive a trap for free. It stops short of AArch64's `x30` and has no
representation at all for a system register.

`TrapFrame` now carries `thread_pointer` and `link_register` after
`prior_depth`, so no existing offset moved and the `pc == 240` / `status == 248`
assembly gate still holds. `system/src/machine/aarch64/trap_entry.S` stores both
before its first `bl` — the only point at which `x30` is still the interrupted
context's — and `__system_restore_frame` writes them back before `eret`. RISC-V
leaves both zero because it already has the values in its register file.

The frame grew from 592 to 608 bytes, which is visible in the boot log as
`STACK_POLICY ... cpu_state=608` and `SYSTEM_STACK_READY ... cpu_stride=608`.
Nothing asserts the old size; `SavedState`, `cpu_state_bytes` and the assembly
`frame_bytes` all derive from `size_of::<TrapFrame>()`.

Still not carried: the floating-point and NEON state. That is a real gap for any
context that traps with live floating-point registers, and user code is built
with `+neon`.

### Fixed: the firmware root's own table could not be recognised as reusable

`SYSTEM_STARTUP_FAILED` came from `system/src/kernel.rs:1610`, i.e. from
`launch()`, not from Finder:

```
SYSTEM_STARTUP_FAILED: executable PTE 0x430fd000 changed: expected 0x0, observed 0x80000040928003; original mapping/backing retained
```

**Decoding the occupant.** `0x80000040928003` is
`Entry::table(0x40928000, _, PageOwnership::Automatic)`: valid, leaf bit set,
output address `0x40928000`, software bit 55 set. It is a *table*, not a leaf.
Above the page level AArch64 writes `0b01` for a block and `0b11` for a table, so
`0b11` here is a pointer to the next level, and `AF` and `SH_INNER` are correctly
absent because a table descriptor has no attributes. Two earlier readings of this
value — "bit 63" and "a leaf with the wrong physical" — were a nibble miscount
(the value has 14 hex digits, so the set bit is 55, which is `SOFTWARE_SHIFT`) and
a misread of the leaf bit.

**Two writers, named by measurement.** Instrumenting `Plan::table` printed, in
order, while publishing the Progress Bar CDEF (the instrumentation was removed
before the fix was committed):

```
PGTBL root=0x40007000 level=1 prefix=0x40000000 addr=0x430fd000 cpu=0xffffffe000007000 idx=0x1  observed=0x0
PGTBL root=0x40007000 level=0 prefix=0x43000000 addr=0x430fd000 cpu=0xffffffe00090f000 idx=0x18 observed=0x0
PGTBL root=0x5092a000 level=1 prefix=0x40000000 addr=0x430fd000 cpu=0xffffffe01092a000 idx=0x1  observed=0x80000040928003
PGTBL NOT-TABLE root=0x5092a000 level=1 addr=0x430fd000 observed=0x80000040928003
```

`0x40007000` is the kernel root and `0x5092a000` the firmware root, both from
`MMU_READY`. The refusal is therefore **not** the `edits` loop; it is
`Plan::table`'s `let Decoded::Table { .., global: false, .. } = ... else
{ Conflict }` arm, whose message is word for word the `edits` loop's. The kernel
root's 1 GiB slot was empty and was created; the firmware root's was not.

* **The owner that was already there.** `system/src/machine/paging.rs:791`
  (`prepare_space`) maps every `boot.regions` entry into the firmware root,
  including the `FIRMWARE_ONLY` ones. The aarch64 bootloader declares the
  firmware heap at `0x52430000` with an identity `virtual_base`
  (`bootrom-aarch64-virt/src/main.rs:1011`), which lies inside the 1 GiB region
  `0x40000000`, so `prepare_space` had to create that level-2 table. The entry is
  written by `crates/mac-kernel-model/src/pages/mapping.rs:339` with
  `PageOwnership::Automatic`, which is what bit 55 records. The kernel root skips
  `FIRMWARE_ONLY` regions (`paging.rs:793`), which is why its slot was empty.
* **The owner that was refused.** `system/src/machine/paging/modules.rs:1746`
  (`Plan::table`), reached from `Plan::add_space` for the firmware root because
  `Plan::prepare` publishes every definition into both permanent roots
  (`modules.rs:1615`).

**Why it did not simply reuse that table.** The reuse arm required the decoded
descriptor to report `global: false`. AArch64 has no global bit on a table
descriptor — bit 11 is `nG` and `Entry::table`
(`crates/mac-aarch64-pages/src/lib.rs:243`) never writes it — so `decode_raw`
reports every aarch64 table as `global: true` and the pattern could never match.
RISC-V's `GLOBAL` is a real bit that `Entry::table` leaves clear, so there the
same pattern matched and `qemu_riscv` never reached the refusal. The shared check
encoded one architecture's descriptor convention and was unsatisfiable under the
other's: the same shape as the `execution_handlers` and RAMFB gates, one level
down.

**The fix removes the test, not the check.** `Decoded::Table` is the complete
statement that a slot holds a table pointer — the encoder derives it from the
descriptor's leaf bit, which is the only thing separating a table from a block at
these levels. Retention is still proved: by `TABLES.lock().find(key)` for a
managed branch, or by `space.tables.node(physical)` for a bootstrap branch the
space's own table owner still holds. The reuse arm therefore still cannot adopt
an arbitrary physical page, `commit()`'s `observed != 0` assertion is untouched,
and nothing unmaps or overwrites a live entry — the firmware root's bootstrap
table is retained and extended with the one level-3 table and one leaf the
definition needs.

**What this establishes.** After the fix `raw-hfs-b` publishes the CDEF, creates
and draws the boot Progress Bar control, and the failure moves past paging
entirely (see *What still blocks `qemu_arm64`*).

### Fixed: the client service call had no aarch64 path, and the entry could not record one

Two independent defects, both invisible until a definition actually trapped.

**The client entry.** `crates/mac-traps/src/lib.rs:30` implemented `invoke` only
under `#[cfg(target_arch = "riscv64")]` and returned `NOT_SUPPORTED` otherwise,
so every `Client` method refused by construction and the boot Progress Bar's
`Draw` reported `BOOT_PROGRESS_REFUSED stage=paint status=-4`. The aarch64 arm
now issues `svc #0` with the number in `x8` and the request and its length in
`x0`/`x1`, returning the result in `x0`. The instruction is emitted inline
rather than called through a System symbol, because an application or definition
is linked as its own module and resolves no resident code address.

**The classification.** `system/src/machine/traps.rs:310` accepted only
`(user_origin, 8)` and `(!user_origin, 9)`, which are RISC-V `scause` values for
ECALL from U-mode and S-mode. AArch64 reports a supervisor call in `ESR_EL1.EC`,
so its `0x56000000` matched neither and every `svc` — and every other synchronous
trap — was classified as a fault. No `ESR_EL1` decoding existed anywhere in the
tree.

**The frame.** Three further defects in `system/src/machine/aarch64/trap_entry.S`
made the entry unable to record a frame at all, which is why none of this had
ever been observed:

* The frame bound compared `frames + depth*stride` against `ctl_top`. `top` is
  the System stack's limit; the frame array is a different allocation in a
  different window (`0xffffffe0…` against `0xffffffd0…`), so the comparison could
  never succeed and every trap took the no-frame report.
* The entry read `prior_depth` instead of writing it, and `__system_restore_frame`
  never put `ctl_depth` back, so the nesting counter could only accumulate.
* The restore reloaded `x2` as the frame base and then read through it, so a
  return read the interrupted `x2` as an address. It also never installed the
  interrupted stack pointer, and never set `SP_EL0` for a frame that returns to
  EL0.

**The one owner.** The shared dispatcher, the scheduler and the private Host gate
name the frame slots a call uses, and those slots are RISC-V's. Rather than a
`cfg` per call site, `system/src/machine/trap_frame.rs` is now the single
statement of the difference, implemented once per architecture and selected at
the machine boundary. It answers: which `cause` a call reports, which slot
carries the number, the argument and its length, which slot returns the result,
which link a new context returns through, where the call instruction is, and
whether the entry must step over it. The RISC-V implementation is the identity,
so that side is unchanged by construction, and `dispatch_frame` still names the
two call causes it names.

`user_origin` is still `status & (1 << 8) == 0`, which on AArch64 is `SPSR_EL1.A`
rather than the exception level. It separates EL0 from EL1 because `A` is set in
privileged execution and clear at EL0, and the observed privileged-fault reports
agree with it, so it was left alone: changing it at the same time as the cause
translation would make a later regression unattributable. `M[3:0] == 0` is the
architectural test and belongs in its own change.

**What it establishes.** `raw-hfs-b` now paints the Progress Bar, loads the
Virtio Gpu and VirtIO RNG modules, attaches the video driver, reaches
`BOOT_SYSTEM_READY` and admits the primary as a single-hart dispatcher:

```
BOOT_PROGRESS completed=5 total=7
BOOT_SYSTEM_READY: services=0xffffffd000a43280
SYSTEM_IDLE_REGISTERED thread=… hart=0 name=System idle
SYSTEM_CPUS_READY primary=0 admitted_peers=0 dispatch=primary-only
```

### Fixed: the entropy driver was gated to RISC-V

The next refusal was not `theme`:

```
PROCESS_SERVICE_REFUSED app=4294967394 class=ClassId([69, 78, 84, 82]) version=1 status=-3
RECOVERY_BOOT_REFUSED service=Finder required provider admission refused status=-3
```

`ENTR` had no provider because `platforms/qemu_arm64.json` declared the
`virtio-rng` device in `emulation.entropy` and did not list its driver in
`install.modules` — the same shape again, a capability declared in two places
with nothing comparing them. Adding it exposed the second half:
`drivers/virtio-rng` gated its device module, its `Driver` export and its
build-script guest contract behind `target_arch = "riscv64"`, although
`machine.rs` names no instruction — the ordering comes from `mac-driver::arch`.
It now takes the form `virtio-gpu` already used: `TARGET != HOST` for the guest
contract and `target_os = "none"` for the lock. `MODULE_PUBLISHED name=VirtIO RNG`
follows, and the provider refusal is gone.

### Fixed: aarch64 had no interrupt source, so the scheduler never ticked

`raw-hfs-b` used to stop silently after `SYSTEM_CPUS_READY`. The last thing the
boot did was `system/src/machine/scheduler.rs:1190`: with the Finder startup
flight pending it selected idle, armed the generic timer, and entered the idle
frame. Every remaining boot step — `FONTS_PUBLISHED`, the launch directory,
`SYSTEM_FINDER_LAUNCH` — is reached only from `scheduler::schedule()`
(`scheduler.rs:1650`), and `schedule()` is reached only from `Dispatch::Schedule`,
i.e. from a trap. The idle loop is `loop { wfi }` (`scheduler.rs:1262`) and has
no other body.

Three defects had to be fixed, and the third was only visible after the first two
were, because a delivered-but-misclassified interrupt and an undelivered one look
identical from the scheduler.

**Classification.** The sixteen vector slots in
`system/src/machine/aarch64/trap_entry.S` all branched to one entry that recorded
only `ESR_EL1`. An IRQ writes no `ESR_EL1`, so the recorded word is whatever the
last `svc` left there, and `ESR_EL1.EC = 0x15` would have made the interrupt look
like a service call. Each slot now states its kind in `x3` — the slot number is
the source times four plus the kind — the entry records it in a new `TrapFrame`
field, and `machine/aarch64/trap_frame.rs` answers two questions from it:
`interrupt(frame)` reads and completes the controller's acknowledgement, and
`cause(frame)` translates `ESR_EL1.EC` to a call cause *only* for a synchronous
trap, returning every other cause unchanged so a genuine fault still reports the
value the hardware produced. The shared dispatcher no longer compares a cause word
to a RISC-V bit pattern; it asks for an `Interrupt`, and `machine/riscv64` reports
its own `scause` values through the same three-way answer.

The frame grew from 600 bytes of fields to 608, which fits the padding the 16-byte
alignment already required: `cpu_state` stayed 608 and no offset moved.

**Delivery.** `system/src/machine/aarch64/gic.rs` is a GICv2 distributor and CPU
interface, and `machine/aarch64/interrupts.rs::admit` resolves it from the
machine's device tree and checks it against the platform record. Both register
blocks and the timer's interrupt number come from the tree; the record declares
only the window the kernel root maps, and a tree block outside that window is a
refusal rather than a fault. The device tree states the non-secure physical timer
as PPI 14, which is **INTID 30** — a distributor enable bit is indexed by the
absolute number, and reading the specifier's number as that number enabled an
interrupt nothing raises. `INTERRUPTS_READY` reports what the machine says after
the writes, not what was written.

**The group register.** With `GICD_CTLR`, `GICC_CTLR`, the priority, the target,
the enable bit and the timer all reading back correct, and the timer armed 87 µs
out, no interrupt was ever taken. The wrong statement was
`GICD_IGROUPR0 = !0`: that is right on a controller with the security extensions,
where a non-secure kernel can only be delivered group 1 and the enable bit it
writes is the non-secure alias of `EnableGrp1`, and wrong on one without them,
where there are no groups, every source is group 0 and `GICD_CTLR` bit 0 is
`EnableGrp0`. Putting every source in a group nothing enables leaves the
distributor forwarding nothing while every register reads back exactly what was
written. `GICD_TYPER.SECURITYEXTN` decides it, and the read-back includes the
group register so the same defect cannot be silent again.

`clear_software_interrupt` and `enable_software_interrupt` are gone from the CPU
adapter. RISC-V's clear moved into its own interrupt classification, which is
where its pending bit is completed; a controller acknowledgement has no equivalent
call, so keeping one would have been a name with no work behind it. Both
architectures now admit the wake source through one operation,
`cpu::admit_wake_source`, which is a CSR bit on RISC-V and the software-generated
bank on AArch64.

**What this establishes.** The tick arrives: 14342 IRQs were taken from EL0 in one
30-second run, and the boot reaches `FONTS_PUBLISHED`, `DISPLAY_MODE_CHANGED`,
`LAUNCH_DIRECTORY_READY`, `SYSTEM_FINDER_LAUNCH entry=0x400000` and
`BOOT_PROGRESS completed=7 total=7`.

## Fixed: `AP[1]` was inverted, and System's frames ran at EL0

Finder was launched and took a data abort on its own stack — permission fault,
level 3, on a write, at `0xfa8fe0` with the stack at `0xfa9000` — and the
recovered instance aborted the same way. `crates/mac-aarch64-pages/src/lib.rs`
encoded `AP[1]` inverted:

```
let ap1 = if permissions.access == Access::Supervisor { 1 << 6 } else { 0 };
```

`AP[2:1]` is the stage-1 access-permission field, and bit 6 is `AP[1]`, the
**unprivileged**-access bit:

| `AP[2]` | `AP[1]` | EL0 read | EL0 write | EL1 read | EL1 write |
|---------|---------|----------|-----------|----------|-----------|
| 0       | 0       | no       | no        | yes      | yes       |
| 1       | 0       | no       | no        | yes      | no        |
| 0       | 1       | yes      | yes       | yes      | yes       |
| 1       | 1       | yes      | no        | yes      | no        |

As written, every application page was EL1-only and every kernel page was
EL0-accessible — read, write and execute. The decoder mirrored the inversion, so
the crate's round-trip tests agreed with the encoder and with nothing else. The
decoder now states the leaf permissions once, in `permissions_from`, and both the
block and page branches use it.

The round-trip test asks the EL0 question of the encoded `AP[2:1]` bits, not of
the input enum, in both directions: a supervisor leaf must encode `AP[1] = 0` and
an application leaf `AP[1] = 1`, and the decoder must read those same bits back.
Before the flip it failed with

```
AP[2:1]=01 on the supervisor leaf grants EL0 access
```

**The frame status words, in the same change.** `INITIAL_STATUS` and
`USER_STATUS` were RISC-V `sstatus` values used unchanged as `SPSR_EL1`.
`INITIAL_STATUS = 0x66120` has `M[3:0] = 0b0000`, so the System idle and
module-worker frames were *entered at `EL0t`* while carrying kernel addresses.
They ran because every kernel page was (wrongly) EL0-accessible; a monitor sample
during the hang reads `PC=ffffffd0005032ac PSTATE=0000000000000300 ---- EL0t`.

The origin test used `SPSR_EL1.A` (bit 8). That bit is the SError mask, not the
exception level: it is always clear at EL0, and clear at EL1 whenever SError is
unmasked, so an EL1 frame would have been classified as an application call. It
appeared to work because the RISC-V words put `SPP` in bit 8 — set in the System
word, clear in the application word — so the test agreed with the intent while
the hardware executed System's frames at EL0.

Both words are now real `SPSR_EL1` values, stated in
`machine/aarch64/trap_frame.rs`:

| word             | `M[3:0]` | meaning                                      |
|------------------|----------|----------------------------------------------|
| `INITIAL_STATUS` | `0b0101` | `EL1h`, DAIF unmasked — System-owned frames   |
| `USER_STATUS`    | `0b0000` | `EL0t`, DAIF unmasked — application frames    |

and the origin test reads `M[3:0]`. The RISC-V words moved unchanged into
`machine/riscv64/trap_frame.rs` behind the same two names and the same
`from_user`, so the shared dispatcher asks one question instead of testing a bit
of its own: `machine/traps.rs` and `machine/scheduler/user.rs` both call
`trap_frame::from_user`.

The other status-bit test had the same shape and needed the same treatment. The
"may this frame be suspended or moved" test in `machine/traps.rs` and
`machine/scheduler/service.rs` tested `1 << 5` by hand. Pointing it at the
existing `page_layout::interrupts_enabled` looked like the obvious
consolidation and was wrong: that predicate answers for a status word read
*now*, and on RISC-V it reads `sstatus.SIE`, which the trap has already cleared.
A saved frame carries the pre-trap state in `SPIE`. The `qemu_riscv` gate caught
it immediately — every application call was refused as `WrongThread` — so each
architecture now states `trap_frame::interrupts_were_enabled` separately from
`page_layout::interrupts_enabled`, and the two questions have two names.



## Fixed: the exception return handed every resumed context the control block in `x4`

The physmap read described above was not an application defect and not a page
permission defect. It was the exception return.

`__system_restore_frame` reloads the interrupted integer file and then, to clear
`Control.entering`, read `TPIDR_EL1` through `x4` — a register it had restored
sixteen instructions earlier and never reloaded:

```
ldp x4, x5, [x2, #{registers} + 32]   // x4 = the interrupted x4
...
mrs x4, tpidr_el1                     // x4 = the control block, for good
str xzr, [x4, #{ctl_entering}]
ldp x2, x3, [x2, #{registers} + 16]
eret
```

`x4` is the fifth argument register. A trap that lands between a caller placing
its fifth argument and the callee reading it therefore delivers the control
block instead of the caller's pointer. That is why three unrelated Finder sites
faulted, why they faulted at different offsets, and why the fault was
intermittent: it needs a trap inside a window of a few instructions. Before
`9f5fd6c9` the read succeeded, because `AP[1]` was inverted and every supervisor
leaf was EL0-readable.

The fix reads `TPIDR_EL1` through `x0` and reloads `x0` afterwards, which is the
ordering the RISC-V entry already uses for its own scratch (`t0`/`t1`):

```
mrs x0, tpidr_el1
str xzr, [x0, #{ctl_entering}]
ldp x0, x1, [x2, #{registers}]
ldp x2, x3, [x2, #{registers} + 16]
eret
```

The rule the two entries now share: **a scratch register may not be one the
frame has already restored, and a register used as scratch after its reload must
be reloaded again.**

### The trace

QEMU's `-d int` names the fault but not the general registers, and `-d cpu` is
unusable here (2.5 GB of trace in 70 s). Two things did work, and they agree:

- the crash report text lives in guest RAM and carries the whole captured
  register file (`FaultSnapshot`, rendered into the report window);
- `system/src/machine/traps/state.rs` keeps the same frame at the address
  `PROCESS_THREAD` prints as `cpu_state=`, so the frame is readable directly.

Dump with the machine **stopped** (`stop` on the monitor, then `pmemsave`);
otherwise the guest reclaims the faulting thread's stack while the 2 GB dump is
still being read, and the stack copy of the frame is gone by the time it is
reached.

Two captures, two runs, two different call sites, the same value:

| | capture 1 | capture 2 |
| --- | --- | --- |
| `PC` | `0x43c600` `Browser::draw_items+0x284` | `0x42bf6c` `Startup::poll_watches+0x6d8` |
| instruction | `ldrh w8, [x23, #0x2]` | `ldr w13, [x23, #0x638]` |
| `FAR` | `0xffffffe010914002` | `0xffffffe010914638` |
| `SP` | `0xf76580` | `0xf83aa0` |
| `x4` | `0xffffffe010914000` | `0xffffffe010914000` |
| `x23` | `0xffffffe010914000` | `0xffffffe010914000` |
| `x5` | `1` | `0` |

`0xffffffe010914000` is the control block the same run prints as
`SYSTEM_STACK_READY … control=`. In both functions `x23` is `x4`-derived: the
first line of `draw_items` is `mov x23, x4`, and `poll_watches` computes
`add x23, x13, x19` from the `&mut Browser` it received in `x4`. The callers put
the argument there immediately before the call:
```
add x4, sp, #0x130        add x4, sp, #0x2b8
bl  draw_items            bl  poll_watches
```

So the caller was correct and the callee was correct; the register in between
was overwritten by the exception return. The same shape reaches the third
recorded site: `Startup::advance` takes its `browser` in `x4` and reads
`browser + 0x1e64` at `advance+0x7b0`, which is the `Cell<bool>` at
`browser.rs:487` named in the earlier crash report.

`FaultSnapshot` prints the 30-slot frame under RISC-V register names, so the
`t1` line of an aarch64 report is `x4`, `s9` is `x23`, and so on; `x30` is not
in the slot file at all. The table above uses the AArch64 names the disassembly
does. Relabelling that report per architecture is a separate defect, not fixed
here.

### What this does not change

The control block is still supervisor-only, the `AP[1]` fix stands, and nothing
was added to make kernel memory reachable from EL0. The fault sites were not
stubbed; they were fixed at the one place that produced the bad pointer.

## The probe fixtures are `std` applications now

`boot-check` builds two probe fixtures while it assembles the case table:
`fonts-probe` and `pipes-probe`. Both were `no_std` applications using
`mac_toolbox::application!`, whose entry scaffold is RISC-V only — the macro
states `compile_error!("System RISC application runtime currently requires its
native RV64 target")`, and the scaffold underneath it is a `no_std` one
(`GlobalAlloc`, `#[panic_handler]`, `#[alloc_error_handler]`, `mac_traps`
application heap and TLS teardown). P8 makes `std` the default for every native
application, so the port was the work, not a widened `cfg`.

Both fixtures are now ordinary `std` applications:

* `fn main() -> Result<(), Box<dyn Error>>` with `System::new()`, no
  `#![no_std]`, no `#![no_main]`, no `application!`.
* `mac-toolbox` without the `application` feature; `mac-build` only as a
  build-dependency. The `[target.'cfg(not(target_arch = "riscv64"))'.dependencies]`
  entry is gone: its predicate meant "not a bare-metal guest", which is also
  true of an aarch64 guest, so it put a host tool with `std` into the target
  dependency graph and the build died in `serde_core`.
* `build.rs` calls `mac_build::build_guest()` unconditionally. The old script
  ran only when `TARGET` was the bare RISC-V triple, so on any other target it
  silently skipped the resource manifest, the generated platform constants and
  the linker script.
* The per-fixture `package` binary is gone; `xtask` packages the fixture with
  the same `mac_build::package::build` call the removed binary made. That binary
  was the second reason `mac-build` had to be a guest dependency.
* The pipes probe's two worker threads are `std::thread::Builder::spawn`. The
  previous `Client::create_native_thread` is documented as the `no_std` CRT's own
  construction primitive and is not std's `Send` handle; a `std` process must
  create threads through the `std` runtime so their TLS is the runtime's. The
  endpoint token is still handed over as an address, with a `Send` marker for
  the address and the creator keeping the token alive until the child reports
  `ARMED`. The children take the platform's default stack, as an application's
  threads do.

`xtask` builds a fixture through the platform's private SDK
(`sdk::standalone_application`): the same target specification, sysroot,
compiler proxy and `relocation-model=pic` flags an installed application uses,
selected by `--manifest-path` because a fixture is its own workspace. A fixture
built any other way would be evidence about a different image than the one the
platform installs.

No probe genuinely needs `no_std`. Nothing else in `crates/mac-toolbox/tests/fixtures`
is built by `xtask`; the remaining fixtures are used by the host suite.

## The pipes probe faults after its first child thread completes

`pipes-probe` on aarch64 stops in the second of its eleven cases. The thread
that is woken after blocking inside a pipe call is the one that faults, and
which thread that is depends only on which of the two threads the wake reaches
first:

With the child on a 64 KiB stack, the child completes its write and the
**creating** thread faults when the child's completion wakes it:

```
PIPES_PROBE_ECHO_OK bytes=5
PIPES_PROBE_WRITER_ARMED
PIPES_PROBE_WRITER_PARKED
PIPES_PROBE_MAKE_ROOM
PIPES_PROBE_WRITER_RESUMED transferred=512

PROCESS_THREAD_RECLAIMED app=4294967394 thread=4294967489
USER_FAULT_CAPTURED app=4294967394 thread=4294967440 cause=4060086272 pc=0x41c0c8 address=0x0 status=0x80000000
CRASH_REPORT_READY app=4294967394 window=4294967529 bytes=1013 lines=40 page_lines=22 flags=0
NATIVE_EXIT_RECOVERY_QUEUED app=4294967394 kind=abort code=255 thread=4294967440 report=SystemDiagnostic ...
```

With the child on the platform's default stack, the **child** faults when the
creating thread's drain wakes it, one marker earlier:

```
PIPES_PROBE_MAKE_ROOM
USER_FAULT_CAPTURED app=4294967394 thread=4294967513 cause=4060086272 pc=0x41c550 address=0x0 status=0x80000000
CRASH_REPORT_READY app=4294967394 window=4294967565 bytes=1013 lines=40 page_lines=22 flags=0
NATIVE_EXIT_RECOVERY_QUEUED app=4294967394 kind=abort code=255 thread=4294967513 report=SystemDiagnostic ...
```

Both PCs are inside `systemrisc_std_traps::emergency::exit_returned` in the
probe's own ELF — `+0x74` in the first build, the corresponding offset in the
second — and that function's last instruction is a deliberate `brk #0`.
`cause=4060086272` is `0xF2000000`: `ESR_EL1` exception class 0x3C with `IL` set
and ISS 0, which is that `BRK #0` taken at EL0 (`status=0x80000000`).
`exit_returned` is reached only from `emergency::abort`, after a PROCESS exit
trap returned instead of terminating the process.

Read what the record does and does not say:

* The pipe data path works. A child writes into a pipe, parks because the pipe
  is full, the creating thread drains it, and the child resumes and transfers
  all 512 bytes. That is `WRITER_ARMED` through `WRITER_RESUMED` in the first
  run.
* The fault is always on the thread that was woken, never on the one that did
  the waking.
* Neither `emergency::abort`'s own diagnostic nor any probe marker between the
  wake and the fault reached the serial log, and neither `NATIVE_EXIT_REFUSED`
  nor `NATIVE_EXIT_PENDING` appears. The published report is the resident's own
  fault report (`report=SystemDiagnostic`, 1013 bytes and 40 lines every time),
  not an application report: `failure::report` appended nothing.
* The same source on `qemu_riscv` continues past this point:
  `PROCESS_THREAD_RECLAIMED` for the child, then `PIPES_PROBE_BLOCK_OK
  parked=1 resumed=1 eof=1`, and the case passes with eleven cases.

So the ordering of the block and the wake is the same on both platforms, and
what differs is what the woken thread does when it resumes. On aarch64 it does
not continue after its `svc`; the first thing it executes that reaches the
record is the terminal-fault path. That is a defect in the resume of a thread
that blocked inside a service call — a path the arm64 gate had never exercised
before, because no earlier case blocks a user thread inside a service. It is not
a pipe defect and not a probe defect.

This was not worked around. The candidate causes are all in the aarch64
exception/resume path (`system/src/machine/aarch64/`, the shared `TrapFrame`
carried by `trap_frame.rs`, and the scheduler's resume), which this change was
not permitted to touch.

What the fault line does not establish: whether the resumed context's program
counter was restored from the wrong frame slot, or whether the resumed thread
ran correctly and then had its first service calls refused. Distinguishing those
needs the resident to name the caller of `emergency::abort`, which it does not do
today. The line that made this diagnosable at all — `USER_FAULT_CAPTURED` — was
added in the same change, because until then a fault named no cause, PC or
address anywhere in the serial record.

## Measured: an aarch64 replay resumes four bytes past its `svc`

The question was where a woken aarch64 thread resumes when the service it was
parked inside asks to be replayed. It resumes at the instruction **after** the
`svc`, not at the `svc`. That is the defect.

A parked service call is not completed when the thread is parked. The dispatcher
returns `RequestAction::Replay` and `Dispatch::Schedule` after recording
`state.service.ecall = Some(Call::new(frame))`, and the thread is switched out
holding its own `TrapFrame`. Nothing commits a result and nothing advances the
frame: the shared contract is that the woken thread **re-executes the call
instruction**, and the service runs again. On RISC-V that holds for free. The
entry stores `sepc`, which names the `ecall` itself, and `trap_frame::advance`
adds four only when a call actually completes.

AArch64's `ELR_EL1` names the instruction *after* the `svc`, and its `advance`
is a deliberate no-op for that reason. So the parked frame's `pc` is already
`call_site + 4`. `abi::call_site(frame)` recovers the call instruction, but only
as an identity for `Call::new`; nothing ever moves the resume address back.

Measured with a one-line diagnostic at the replay park, on
`cargo xtask boot-check --platform qemu_arm64` at `79bb1826`:

```
PIPES_PROBE_WRITER_ARMED
EXITDIAG_REPLAY number=65583 pc=0x400c7c call_site=0x400c78 prior_depth=0
PIPES_PROBE_WRITER_PARKED
PIPES_PROBE_MAKE_ROOM
EXITDIAG_REPLAY number=65578 pc=0x41be2c call_site=0x41be28 prior_depth=0
USER_FAULT_CAPTURED app=4294967394 thread=4294967495 cause=4060086272 pc=0x41c550 address=0x0 status=0x80000000
```

`65583` is `PIPES` and `65578` is `NATIVE_THREAD` (`crates/mac-abi/src/traps.rs`):
the child parks in its blocking pipe write, and the creating thread parks in
`Thread::join`. In both, `pc` is exactly `call_site + 4`. Two runs of the same
case produced the same two pairs with the same four-byte difference.

The consequence is the record the previous section describes. The woken thread
does not re-execute the call, so `state.service.ecall` is never completed, and
the thread carries an outstanding call obligation it can no longer satisfy.
Every later call then reaches `enter_ecall` with `original != Call::new(frame)`.
That test exists precisely to refuse a mismatched obligation with
`ACCESS_DENIED`, and it is the one branch that can refuse a call before any
service body runs — which is why nothing between the wake and the fault reaches
the serial record: the thread's own diagnostics are service calls, and they are
refused too. It ends at the `BRK` in `emergency::exit_returned`, exactly as
`USER_FAULT_CAPTURED` reports.

This also names the desktop symptom. An application that quits after any service
parked it cannot complete its own exit, because the exit is one more service
call. The same refusal explains a Finder window close that stops responding.

The fix is the missing counterpart of `advance`. `advance` means "the call is
finished, resume after it": RISC-V adds four, AArch64 does nothing because
`ELR_EL1` is already past. `rewind` means "resume at the call so it is issued
again": RISC-V does nothing because `sepc` already names the `ecall`, AArch64
subtracts four. The two functions now state the whole convention, once per
architecture, beside each other. `finish_service` keeps calling `advance`; the
two deferred-replay returns — `wait_io` reporting the thread parked, and
`RequestAction::Replay` — call `rewind` first. No other `Dispatch::Schedule`
takes it: preemption and fault capture resume at a genuine interrupted
instruction, migration resumes on another hart, and `Yield`, `Exit`, the blocked
event wait and the worker park have all already committed their result through
`finish_service`.

The contract the code had assumed is stated at `finish_service`: *deferred
replay intentionally never calls this helper*. That sentence is true on RISC-V,
where skipping `advance` leaves the PC on the `ecall`. It was not true on
AArch64, where there was nothing to skip and nothing that moved the resume
address back.

The stale-`Call` diagnostic in `enter_ecall` never fired while this was being
measured, so the exact terminal refusal that follows a missed replay is still
inferred from the code rather than instrumented. The resume address, which is
the defect, was measured directly.

### The result on `qemu_arm64`

With `rewind` at the two deferred-replay returns, `pipes-probe` no longer faults
and no longer crashes. It runs from `PIPES_PROBE_ECHO_OK` through
`PIPES_PROBE_SCAN_RESTART_OK entries=2`, and the case it used to die in now
passes outright:

```
PIPES_PROBE_ECHO_OK bytes=5
PIPES_PROBE_WRITER_ARMED
PIPES_PROBE_WRITER_PARKED
PIPES_PROBE_MAKE_ROOM
PIPES_PROBE_WRITER_RESUMED transferred=512
PROCESS_THREAD_RECLAIMED app=4294967394 thread=4294967528
PIPES_PROBE_BLOCK_OK parked=1 resumed=1 eof=1
PIPES_PROBE_READER_ARMED
PIPES_PROBE_READER_PARKED
PIPES_PROBE_READER_RESUMED transferred=5
PIPES_PROBE_READER_OK parked=1 resumed=1 bytes=5
```

No `USER_FAULT_CAPTURED` and no `CRASH_REPORT_READY` appears anywhere in that
log. `PIPES_PROBE_BLOCK_OK` is the exact marker the woken-writer case produces
on `qemu_riscv`.

The case still does not pass, and the reason is a different defect:
`PIPES_PROBE_FAIL the Pipes folder has no icon of its own`, raised by the
`PIPES_ICON_FOLDER_OK` assertion after `PIPES_SCAN_RESTART_OK`. That is an
icon/glyph coverage gap on this platform, not a resume defect, and it is not
touched here. The arm64 case count therefore stays at 3 of 19: `no-disk`,
`raw-hfs-b` and `fonts-probe`. What changed is that the first failure is no
longer the resume path — the probe now reaches the same point in its own
coverage that the icon assertion guards.

### The result on `qemu_riscv`

`cargo xtask boot-check --platform qemu_riscv` passes 19 of 19 with `rewind` in
place. That is the comparison: the same source, the same case, and a `rewind`
that is a no-op on this architecture because `sepc` already names the `ecall`.
Nothing on RISC-V moved, and the blocking-writer case still reaches
`PIPES_PROBE_BLOCK_OK` and the case its eleven markers.

The RISC-V half of the resume address was established from `sepc`'s definition
and from this run, not from the removed diagnostic: the instrumented build was
aarch64 only.

## Not attempted, deliberately

The floating-point/NEON part of an aarch64 thread context. `TPIDR_EL0` and `x30`
are now carried (see *What still blocks `qemu_arm64`*); the 32 128-bit vector
registers and `FPCR`/`FPSR` are not. That is one change with one owner — the
thread/std boundary — and user code is built with `+neon`, so a context that
traps with live floating-point state still loses it. Carrying the two scalar
values was a prerequisite for `std` TLS and for a resumable return link; carrying
the vector state is a prerequisite for preempting user code at all, and it needs
a frame wider than the shared RISC-V-shaped vector area.

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
