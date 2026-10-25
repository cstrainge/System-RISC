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

## Qemu Power Device

`drivers/qemu-power-device` builds an external `DRVR` file named **Qemu Power
Device**, installed in the blessed `System Folder/Drivers` directory. Its DABI
contract declares `POWR` capabilities and a match for the platform-described
SiFive controller. It claims that device through the normal `VDEV` service;
there is no hard-coded MMIO address in the package. The safe request engine
rejects unsupported classes, phases, operations and payloads before touching
hardware. Only the small machine adapter accesses registers.

The QemuRISC-V JSON platform definition lists MMIO devices, and its fatal signal
references that same checked device record. Build validation rejects malformed,
overlapping, misaligned and RAM/flash-overlapping ranges. One record emits both
discovery constants and the existing fatal-signal linker symbols. The device
service's slot count lets drivers enumerate inventory without assuming a board's
number of devices.

The driver writes the SiFive finisher's pass value for shutdown and reset value
for restart. These meanings come from QEMU 10.1's
[device implementation](https://github.com/qemu/qemu/blob/v10.1.0/hw/misc/sifive_test.c)
and [register constants](https://github.com/qemu/qemu/blob/v10.1.0/include/hw/misc/sifive_test.h).
Normal power requests always use the selected external driver. The existing
panic-time fatal signal remains independent of ordinary services: arbitrary
privileged-fault containment is unproven (M-D04), a required future gate.

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
