# Built-in block storage and loadable kernel drivers

Status: design/backlog, tracked as [P14](CORE-TOOLBOX-ROADMAP.md#p14--loadable-kernel-drivers).
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
