# Built-in block storage and loadable kernel drivers

Status: the initial class-neutral contract/registry/relocating module loader is
implemented; storage class providers and deferred I/O remain in progress, tracked as [P14](CORE-TOOLBOX-ROADMAP.md#p14--loadable-kernel-drivers).
The minimal block-device path remains built into System for boot/storage access.
Other device backends move into separately packaged drivers loaded through P6's
extension machinery. These modules execute in the kernel, not isolated driver
processes; they are part of the trusted kernel described by P13.

## Installation and discovery

Install native driver packages in the selected blessed System Folder's `Drivers`
directory (`System Folder/Drivers`). System extensions use the separate
`System Folder/Extensions` directory. The shared kernel module machinery does
not make these installation locations interchangeable: discover drivers only
in Drivers and extensions only in Extensions. Report packages placed in the
wrong directory without invoking their code; do not search the other directory
as a fallback. Placement in Drivers does not replace DRVR/DABI validation.

The system-image builder must create both directories and place packages by
class when disk-based module loading is implemented. Resolve declared
dependencies across the two discovered package sets through the common module
registry, rather than treating directory enumeration order as startup order.
The built-in bootstrap block provider does not require a disk package.

Boot discovery takes one bounded snapshot of the platform's device inventory.
A structurally valid, otherwise admissible package with declared device matches
is loaded only when at least one match is present. A package with no matches is
hardware independent. The generic loader compares bus IDs and masked keys;
the platform adapter alone knows MMIO addresses and slot counts. Presence does
not grant ownership: claiming a resident-owned or already-claimed device still
fails through the normal service checks.

Absent-device packages are reported as `MODULE_SKIPPED`, with no native entry
or published interface. Required dependencies propagate the skip only when their
matching providers are absent and no ready or waiting alternative can satisfy
them, independent of directory order. Optional dependencies do not propagate a
skip; wildcard dependencies remain eligible for another provider. Wrong ABI, target,
installation directory and malformed contracts remain rejections. Missing or
cyclic dependencies without a known absent device remain explicit failures.
This is boot discovery, not hotplug detection or a per-event device scan.

## DRVR package and mandatory contract metadata

Only a driver package marked `DRVR` may enter the driver-loading path. Use DRVR
file classification and a native `DRVR` code resource, with mandatory contract
metadata in a companion `DABI` data resource. Neither a filename nor finding
legacy DRVR bytes during resource inspection authorizes execution. Keep the
code's existing native envelope/segments/RLOC; DABI does not replace NABI or
change the unreleased version-1 executable format into ELF.

Generate DABI from the actual SDK/target/build contracts through mac-build;
do not require hand-maintained version strings that can drift from linked code.
Define its versioned binary layout in FORMAT.md when implementing the loader.
Required metadata includes:

- Metadata magic/schema and unique module identity, Cargo version and diagnostic
  build provenance; identify the intended native driver entry resource explicitly.
- Driver entry/API major/minor compatibility and exact versions of required
  kernel service contracts, with required versus optional capabilities.
- Target architecture, pointer width, endianness, calling convention and CPU/ISA
  requirements, consistent with the code's native descriptor.
- Device/bus matching and provided device-class interfaces, dependency/startup
  phase requirements and any exclusive-provider rules.
- Entry/request/response layout versions and sizes, callback execution/context
  rules and synchronization/blocking contract. No Rust trait objects, private
  struct layouts or allocator-owned values across the module boundary.

Before invoking any entry or allowing device claims, check package marker,
exact required metadata cardinality, supported schema/API, native descriptor,
target, bounds/relocations, entry reference, dependencies and required services.
Reject missing/conflicting/unsupported requirements with a specific loader status.
Unknown optional capabilities may be negotiated only by the documented rule.
DRVR is a format/type marker, not a cryptographic signature; trust/installation
policy is separate from contract compatibility and does not follow from FourCCs.

## Driver interfaces and ownership

Provide a small no_std Rust driver SDK over P6's typed resident interfaces,
with audited MMIO/DMA/interrupt adapters and safe ordinary driver logic. Device,
request, interrupt, DMA-map and registration IDs use the shared generation-tagged
facility with central namespaces and separate ownership checks. Validate claims
and late completions against their originating driver/device incarnation.

The module/driver core is class-neutral: it owns contract validation, discovery,
dependency resolution, loading, registration, ownership and lifecycle. Class
interfaces are identified and versioned independently; adding a filesystem,
block, input, network, display, audio or other class must not change the core
entry contract or assume that every device has sectors or catalog operations.
A module may provide several class interfaces, and a transport module may be a
dependency of a separate functional driver. Class-specific request layouts,
capabilities and execution rules belong to that class's SDK contract.

The core owns enumeration, matching, queues, lifetime and service registration;
drivers own device-specific operation. Keep block, input, network and display
class interfaces separate from each bus transport. Native apps and disk tools
use public Toolbox/block/file services, never private driver globals. Preserve a
separate classic Device Manager/DRVR adapter track rather than treating legacy
headers and native DABI as interchangeable entry contracts.

Filesystem backends also use explicit contracts: block-backed volumes consume
the block class; [StuffIt archive volumes](ARCHIVE-VOLUMES.md) consume a leased
backing file. [Vhost shared directories](HOST-FOLDERS.md) have their own
filesystem/block-device driver pair. The lower driver owns transport and device
I/O; the upper driver owns fork-container and guest catalog semantics. Directory
exports require a negotiated directory/file-operation service, distinct from the
block interface for actual disk exports. Describe these provided/required class
contracts in DABI and validate dependencies before publishing a volume. Neither
archive mounts nor host directories should be forced into a fake sector model.

Define bounded interrupt acknowledgement and deferred work, buffer ownership,
I/O cancellation/timeouts, power/reset/error behavior and thread-safe publication.
Do not call app code under driver locks or run slow device work with global
interrupt masking. Pin modules until reboot initially. Device reset/removal must
still quiesce requests and DMA safely; pinning code alone does not solve stale
device state. Hot module unload remains a later P6 lifetime gate.

## Boot handoff and extraction sequence

ROM retains enough block support to locate/read the blessed System. System's
built-in block driver takes over via an explicit quiesced handoff, preserving
volume/device identity and preventing two owners from driving the same queues.
Do not leave normal runtime storage dependent on callable raw M-mode firmware
pointers after P13. Optional disk-based driver discovery must not be needed to
load the block driver that reads those same files.

1. Define driver contracts/DABI validation, the resident registry and built-in
   block-provider interface. Test ROM-to-System handoff and boot failure paths.
2. Prove the filesystem and device contracts with loadable StuffIt and external
   vhost mounts (P16/P17), including the vhost filesystem/block-device pair.
   Bring in the required P6 loader/dependency/service subset; the complete
   control-panel suite is not a prerequisite. Keep native HFS-B and bootstrap
   block storage built in. Exercise malformed modules, I/O failure, leases,
   cancellation and stale IDs against real providers before freezing the SDK.
3. Add the [host clipboard extension](HOST-CLIPBOARD.md) through P6's typed Scrap
   and host-bridge services, validating the extension interface beyond storage.
4. Extract input, preserving mouse/key timing and device reset behavior.
   Keep emergency diagnostics available if an optional driver fails.
5. Extract network and display through their class interfaces with P7 and the
   compositor. Minimal bootstrap display/diagnostics may remain until a validated
   replacement is ready; it must not become a permanent duplicate driver stack.
6. Add other device classes and additional hardware through the same contracts.
   Common class interfaces can first serve built-in implementations while P6
   loading is developed; dynamic packaging must not fork service semantics.

Missing/incompatible modules must produce explicit unavailable-device states or
the required boot failure, not silent partial initialization. Publish readiness
only after successful staged registration. Use safe boot/disabled-extension
recovery without disabling the built-in block path needed for recovery itself.

## Acceptance

Canonical management/service assertions belong in `crates/mac-toolbox/tests`;
private QEMU fixtures exercise real entry/interrupt/I/O/handoff. Test required
DABI presence, malformed/conflicting descriptors, wrong architecture/ABI, missing
services, bad entry references, relocation and rejection before side effects.
Prove enumeration alone never executes DRVR and inspection in ResEdit stays data
only; add a resource-based DABI viewer showing required and available contracts.
Verify discovery from the selected blessed installation's Drivers directory,
rejection of misplaced packages without entry, and dependency resolution with
extensions discovered separately from Extensions.
Test duplicate/exclusive claims, init rollback, stale completions, timeout/reset,
driver-disabled boot, block durability and cross-app operation. Keep each module's
code/data lifetime, one-hart/SMP synchronization, resource bounds, interrupt/queue
latency and Cursor Test responsiveness in the acceptance report.


## Initial implementation evidence

`mac-module` owns the allocation-free contract codec, open class identifiers and
admission rules. `mac-driver` supplies the borrowed-buffer C boundary and safe
Rust driver body/dependency client. `mac-build` generates and verifies linked
contracts and emits native DRVR/DABI (or INIT/EABI) resources. The resident
`system/src/drivers` registry owns staged registration, generations, dependency
resolution and request state; callbacks execute outside registry/File Manager
guards. The current machine adapter reserves a module arena disjoint from the
ROM's still-live storage heap and all application/definition images.

The `mac-toolbox/tests/driver_modules.rs` private-guest gate loads a test provider
and dependent client linked elsewhere. Their module SDK call verifies returned
bytes before the client can publish. Eight rejection boots cover missing and
duplicate metadata, wrong installation directory, ABI, target and layout,
code-digest mismatch, and malformed relocations with an updated digest. Rust
requires rejection before any module entry and a working desktop in every case.
Both provider/dependent catalog orders are exercised. Further boots cover absent
device IDs, an unavailable bus, dependent skips, a usable wildcard alternative and the actual Host
Transport/Host Filesystem pair with no VM host channel; matching-device boots
still enter and publish both fixtures.
The gate covers the kernel module SDK, not userspace filesystem mounts. The
registry additionally tests hidden initialization, rollback/slot reuse, foreign
identities, exclusive claims, declared dependencies, active requests and quiescing.

This foundation does not complete hardware driver extraction, asynchronous I/O
scheduling, DMA/interrupt ownership, reset/timeout handling, or hot unloading.
The forthcoming host/StuffIt class gates must qualify those portions they use,
including cancellation and failure cleanup, before they can be called working
filesystem providers. Declared heap/stack limits are admission checks rather
than allocation quotas or isolation of trusted kernel code.

## Resident device-service foundation

The module registry now publishes real resident services through the same
interface/request facility. Required services are validated before entry;
module dependencies and resident requirements remain distinct. The `VDEV` v1
class has its own `mac-device-interface` layout and thin `mac-driver::devices`
binding. Its safe resident manager owns generation-tagged claims and DMA maps,
with a narrow QEMU virt MMIO bus adapter. It checks DABI bus matches, module RAM
bounds and owners, and refuses map release before stopped acknowledgement.
Polling/cancellation reset requests retain their broker ticket until terminal.
Firmware block devices and resident input are reserved, so modules cannot steal
them while extraction is incomplete. See [FORMAT.md](../FORMAT.md#resident-device-service-vdev-version-1).

Unit tests cover resident-service declarations, ownership, stale/foreign/reused
claims/maps and deferred reset with live DMA. The private native module gate
also resolves/calls the real service and proves that an admitted matching driver
cannot claim firmware-owned boot storage. This is not evidence of console DMA,
a mounted host directory, archive decoding or complete device reset/hotplug
support; those have separate provider acceptance gates.

### Current device and deferred-request checkpoint

The resident `VDEV` service now enforces declared bus matches, independent device
ownership, generation-tagged claim/mapping IDs, admitted module-memory DMA bounds,
and reset acknowledgement before mapping/claim release. Firmware-owned block and
input devices cannot be claimed by a driver. The private module gate invokes the
real resident service and proves that the boot disk remains protected. It is now
included in `cargo xtask check` as well as its standalone gate.

`drivers/vhost-transport` contains the first production transport module. Its
safe packet engine has host tests for bounded fragments, borrowed-input lifetime,
initial connection retry, cancellation drainage and link/protocol failure. The
RV64 MMIO/DMA adapter packages as `DRVR` + `DABI`. The ordinary image installs
Host Transport and Host Filesystem in `System Folder/Drivers`; both remain
external, independently loaded files. The private native filesystem gates pass
requests through these production modules to the real Linux host companion.
Without the optional host channel, discovery skips both packages and boots
normally. The launcher supplies a default Transfer export and accepts additional
explicit exports. After discovery, resident File Manager enumerates and mounts
the provider's exports before starting userspace; Finder has no special mounting
path. A bounded startup wait retains pending cancellation and cleanup for the
normal worker pump if a provider times out.

`system::drivers::jobs` is the safe owned-request queue for subsequent resident
integration. Jobs use the shared generation facility, retain bounded owned input
and output buffers, dispatch outside queue guards, and keep live tickets until
completion/cancellation acknowledgement. Tests cover exit/discard during pending
I/O, cancellation during a callback, actual mutation results after cancellation,
foreign and reused IDs, separate owners, fairness and memory limits. Scheduler
parking, the runtime pump, filesystem provider and mount/read/write SDK acceptance
remain the next integration work; this queue alone does not implement them.
