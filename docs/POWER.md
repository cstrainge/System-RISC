# System power services and external devices

Applications request `System::power().shutdown()` or `restart()` through the
public Toolbox. Finder's resource-based Special menu uses that API and enables
only the actions reported by `capabilities()`. Neither Finder nor the SDK names
a driver or accesses hardware.

System selects a live `POWR` interface with version 1 and the capability required
by the requested action. Different providers can supply shutdown and restart;
an unrelated or incompatible version cannot shadow a compatible provider.
Missing capability and incompatible contract are distinct refusals. Providers
publish their capabilities in their module contract, so removing the package
from the next boot's Drivers folder removes its power service too.

Before triggering hardware, System requires other applications to be closed,
refuses unfinished file transactions and provider activity, and flushes every
writable block device. Provider commits already acknowledge their own durability
barriers. A failed barrier never reaches the power device. The trigger executes
one bounded, exact owned driver job inside the same non-preemptible service
invocation; unrelated callbacks are not pumped. An unexpected pending/refused
callback leaves its real job owned through cancellation. Ordinary failures leave
System usable and can be retried.

The native reply preserves the failure stage (selection, applications, storage,
or device) alongside the trap status. The Rust error retains typed action,
stage and cause, with actionable display text. For example, a storage flush
failure cannot be mistaken for a missing power driver.

Coordinated application Quit/Cancel negotiation remains a lifecycle gate. The
current service explicitly asks users to save documents and quit other apps;
it does not terminate them or silently discard their unsaved state. Capabilities
are captured when Finder installs its menus; live module replacement and its
notification-driven menu refresh are separate lifecycle work.

## Device contract

`mac-power-interface` defines class `POWR`, version 1:

- Capability bit 0: shutdown; bit 1: restart.
- Operation 1: shutdown; operation 2: restart.
- Empty request and reply. Completion acknowledges issuing the hardware trigger.
- A trigger must finish its bounded invocation. It must not return Pending after
  issuing an irreversible action.

The generic module loader/broker still owns admission, generation-tagged request
identities, serialization and lifetime. Class-specific behavior lives in the
provider. Future hardware drivers use this contract without changes to Finder.

## Power controllers, one per machine mechanism

`drivers/sifive-test-power` and `drivers/psci-power` each build an external
`DRVR` file installed in the blessed `System Folder/Drivers` directory —
**Platform Power Controller** on the RISC-V machine and **PSCI Power Controller**
on the AArch64 one. Each declares the same `POWR` capabilities for shutdown and
restart, and each is named for the mechanism the machine actually has rather than
for the emulator that models it.

The request engine is shared. `mac_driver::power` owns the class, phase,
operation and payload validation and the single `Hardware::trigger` entry point,
so a shutdown and a restart are one mechanism per machine and a malformed request
is refused the same way whichever machine answers. Only the adapter reaches
hardware.

Two packages rather than one with two adapters, for a reason in the loader rather
than in taste: a module's authority to claim a device is its declared match list,
and a package that declares matches is loaded only where one of them is present.
The RISC-V mechanism is a claimable window and must declare the match to be
granted the claim; the AArch64 mechanism is a firmware function with no window
and must not declare one, or the loader would look for a device the machine does
not have and skip the package — the disabled items this work removes. One
contract cannot say both, so each mechanism has one.

- `drivers/sifive-test-power` (RISC-V) claims the platform-described SiFive test
  window through the normal `VDEV` service, and writes the finisher's pass value
  for shutdown and reset value for restart. There is no hard-coded MMIO address in
  the package. The meanings come from QEMU 10.1's
  [device implementation](https://github.com/qemu/qemu/blob/v10.1.0/hw/misc/sifive_test.c)
  and [register constants](https://github.com/qemu/qemu/blob/v10.1.0/include/hw/misc/sifive_test.h).
- `drivers/psci-power` (AArch64) issues PSCI `SYSTEM_OFF` (`0x84000008`) and
  `SYSTEM_RESET` (`0x84000009`) as 64-bit calls over the conduit the machine's
  own tree declares. It claims nothing: a firmware interface has no register
  window.

The RISC-V JSON platform definition lists MMIO devices, and its fatal signal
references that same checked device record. Build validation rejects malformed,
overlapping, misaligned and RAM/flash-overlapping ranges. One record emits both
discovery constants and the existing fatal-signal linker symbols. The device
service's slot count lets drivers enumerate inventory without assuming a board's
number of devices.

### The conduit is declared by the machine

A `/psci` node describes a function and states no register window, so it is not a
device the loader can match or claim, and the driver's contract carries no
device match. Admission therefore follows successful initialization, and
initialization is the capability test: the module resolves the device service,
asks for the declared conduit, and refuses to start unless it implements it.

Where the conduit comes from, in order:

1. `bootrom-aarch64-virt` reads `/psci` from the tree the machine supplied and
   reports `BOOT_AARCH64_PSCI: method=hvc` (or `absent` when the machine has no
   such node). A method the PSCI binding does not define is reported by name.
2. It passes the declared value to System in `BootInfo.power_conduit`.
3. The resident relays that boot fact through the device service (`CONDUIT`); it
   performs no PSCI interpretation of its own and issues no instruction.
4. The driver implements `hvc`. `smc`, `other` and `none` are refused **by name**
   in the serial record and the module declines to initialize, so no `POWR`
   provider is published and Finder leaves the items disabled. A machine that
   declares a conduit this System does not implement therefore keeps the honest
   disabled state rather than receiving a request that cannot be honoured.

The PSCI call itself lives in the driver's machine adapter, never in `system/`.
Normal power requests always use the selected external driver. The existing
panic-time fatal signal remains independent of ordinary services: it uses the
record's finisher on a machine that has one, and the shared reported-halt path on
a machine that does not. Arbitrary privileged-fault containment is unproven
(M-D04), a required future gate.

New driver binaries can use the SDK's `driver_panic_handler!` scaffold. It emits
bounded, allocation-free diagnostics and enters System's reported privileged
fault shutdown, rather than spinning silently or claiming recovery. Driver
quiescence retains its claimed device if release fails, so failure cannot publish
a falsely released controller.

## Verification

Canonical public SDK tests live in `crates/mac-toolbox/tests/power.rs`. They cover
capability/version selection, missing drivers, informative typed refusals,
unfinished writes, running applications, real flush failures and successful
retry. Private physical-input cases exercise Finder commands, restart back
through the ROM, shutdown through QEMU, app-refusal dialog recovery and boot
without the external package. Capture scripts transport evidence; Rust owns
acceptance assertions. These checks do not claim the deferred Quit/Cancel
negotiation or arbitrary privileged-fault recovery gates.

### What has actually been observed

On `qemu_arm64` (QEMU 11.1.0, `system7.hfsb` built from this tree), driving the
Special menu through QMP:

- `BOOT_AARCH64_PSCI: method=hvc` — the conduit read from the machine's tree.
- `MODULE_PUBLISHED name=PSCI Power Controller`, then
  `FINDER_POWER_CAPABILITIES shutdown=true restart=true`.
- Shut Down: `POWER_REQUEST: action=shutdown psci=0x84000008 conduit=hvc`, then
  the emulator exited with status 0. Restart: `psci=0x84000009`, then a second
  ROM banner and a second `DESKTOP_READY:`. No panic or trap in either.

On `qemu_riscv`, `cargo xtask boot-check` reports exactly 19 of 19, and its
`raw-hfs-b` serial log carries `MODULE_PUBLISHED name=Platform Power Controller`
and `FINDER_POWER_CAPABILITIES shutdown=true restart=true`.

**Not observed, and therefore not claimed:** a machine whose tree declares
`smc`, an undefined method, or no `/psci` node. Those paths are implemented —
each refuses by name and declines to initialize, which leaves the items disabled
— but no run in this environment produced such a machine, so they remain
code-reviewed only. The `qemu_arm64` boot-check still stops in `pipes-probe`
(a pre-existing arm64 defect unrelated to power) after `no-disk`, `raw-hfs-b`
and `fonts-probe` pass.
