# Control panels and System extensions

Status: design/backlog, tracked as [platform workstream P6](CORE-TOOLBOX-ROADMAP.md#p6--control-panels-and-kernel-extensions).
No kernel-extension loader or native control-panel API is implemented by this
document. Native Rust extensions may execute in the kernel; their configuration
UI remains an application using the public Toolbox.

[P14 driver extraction](DRIVERS.md) uses this module machinery for in-kernel
device drivers. The minimal block path stays built into System; other backends
become modules. Driver loading additionally requires the DRVR marker and generated
DABI contract metadata before entry. These modules remain trusted kernel code,
distinct from [P13 isolated applications](PROCESS-ISOLATION.md).
Install driver packages in the blessed `System Folder/Drivers` directory;
System extensions go in `System Folder/Extensions`. Shared module machinery
does not make the two discovery paths or entry contracts interchangeable.

The agreed first implementation sequence is P15 built-in HFS-B/tooling, the
necessary driver/module loader and SDK exercised by P16 StuffIt and P17 vhost
mounts, then the [P18 host clipboard extension](HOST-CLIPBOARD.md). Use the latter
to prove typed resident hooks, host-capability detection and extension lifetime
beyond storage. Deliver the required loader/service subset before the broader
control-panel suite; these initial kernel modules do not depend on userspace std.

## Native control panels

Native panels are resource-based Rust applications installed in the blessed
System Folder's Control Panels directory. Each application runs on its own OS
thread, with owned windows/controls, resource menus/icons, Cargo-derived version
resources and System-owned crash reporting. Finder launches them through the
Process Manager. The planned Apple menu provides access to Control Panels;
panel discovery must not depend on Finder implementing their settings logic.

A panel may configure a built-in service, a kernel extension, or both. Identify
the service by stable package identity and a versioned settings contract, not
by a process pointer or file path. Discover availability and distinguish live
changes from changes requiring restart. An absent/disabled/incompatible companion
must yield a useful status rather than a successful-looking no-op.

Store all system settings, enabled-extension selections and panel preferences as
versioned JSON through a resident typed preferences service. Define schema
validation, defaults, concurrent-writer conflict handling and commit/
rollback behavior. Define when changes take effect and how Cancel/revert works;
notify subscribers after commit without invoking code under the storage lock.
Persistent writes and publication must use real File Manager transactions and
report errors. No private preference files that bypass shared synchronization.
Earlier unreleased preference schemas may be rejected/reset explicitly under the
[project compatibility policy](../ARCHITECTURE.md#unreleased-compatibility-policy);
they do not require migration code. Report the reset or required recreation.

## Resource packaging and build

- Package native extension code as independently linked executable resources in
  classic resource-fork files installed in System Folder/Extensions. Declare a
  native `INIT` entry and its extension calling convention explicitly; driver
  packages are installed separately in System Folder/Drivers and use the P14
  DRVR entry and mandatory DABI contract. Never cast an application or WDEF entry
  to either ABI. Metadata declares stable
  identity, Cargo version, CPU/ABI requirements, dependencies, startup phase,
  provided services/hooks and optional panel/settings linkage. Resolve the exact
  metadata layout in FORMAT.md when implementing it, retaining provenance here.
- Reuse mac-build, `resources.json`, the compact native segment format and RLOC.
  Keep ELF at build time. Validate entry/relocation ranges, executable bounds,
  metadata and service compatibility before execution. Allocate checked module
  spans and pin code/data/BSS for the boot session; do not assume the current
  definition pool has enough space for arbitrary extensions.
- Native packages follow the existing rebuild-together ABI policy. A versioned,
  size-checked C-compatible entry/service boundary sits beneath ergonomic Rust
  wrappers. Separately linked modules must not exchange Rust trait objects,
  compiler-private layouts or allocator-owned values without an explicit ABI.
  Keep machine-dependent entry, cache and hardware adapters separate for x86.
- Publish a sample extension and companion panel with documented build/install
  steps. Installing or rebuilding a module must not require editing System's
  source or statically linking its implementation into every application.

## Rust extension services

Plan a small `no_std` extension SDK, distinct from the application Toolbox
facade. It exposes safe typed initialization/context access, fallible allocation,
resource reads, logging, settings and service/hook registration. Implementation
lives in resident System modules; applications enumerate/configure extensions
through thin Toolbox bindings re-exported by `mac-toolbox`. Extension code calls
validated resident interfaces rather than re-entering application ECALLs with a
fabricated AppId or borrowing arbitrary private manager state.

The lifecycle registry owns generation-tagged extension and registration IDs
using the shared handle facility and centrally assigned namespaces, with
separate extension ownership validation.
Stable package names used for preferences are not runtime capabilities. Owned
registration guards support rollback and explicit revocation; prevent stale
callbacks/service clients from aliasing a later registration. Code remains
resident while any callbacks or references can reach it.

Register explicit typed service endpoints and bounded hooks at supported
subsystems. Define hook order, version, arguments, cancellation/reentrancy and
return policy; detect incompatible/exclusive providers. New endpoints use the
native service namespace and never occupy classic-owned traps. General trap
patching is not the native extension interface. OS consumers validate the same
service contract whether reached through native or legacy adapters.

The 2026-10-23 first-cut review ratifies **synchronous participants**. The earlier
workspace extension spec §5.5 rule using queued participant replies, deadlines
and default-on-expiry is **superseded**. A participant runs synchronously in
deterministic order, returns a validated transform/veto, and a failure fails the
request with an informative typed cause. Observers remain read-only and queued
to their own thread/context; they have no veto/reply path. See the
[trait design](EXTENSION-TRAITS-DESIGN.md) for the algorithm. This ruling does not
claim arbitrary privileged-fault recovery; controlled resource cleanup and
System-fatal native memory corruption remain distinct.

Specify execution context for every callback: boot initialization, preemptible
kernel work, or narrowly bounded interrupt-safe work. General work must not run
inside the current globally interrupt-masked trap path. Worker/blocking support
depends on the scheduler gates in THREADING.md. Interrupt callbacks cannot
allocate, block or invoke UI; defer work to a bounded queue and an eligible
worker. UI requests go to the panel/system UI thread through owned messages.
Never call extension code while holding unrelated resident manager locks.

Safe wrappers and manager logic should forbid unsafe code; loader/FFI/hardware
operations stay in audited adapters. Require explicit synchronized shared state
and correct Send/Sync bounds for cross-thread work, with single-hart and future
SMP behavior defined. Kernel modules are trusted code in the kernel's fault
domain: Rust interfaces reduce mistakes but do not isolate a panic or bad native
code. This is not an MMU-isolation or recoverable-fault guarantee.

## Startup, failure and lifetime

The boot ROM still locates and boots the blessed System. System discovers enabled
extensions from that installation's Extensions directory and drivers separately
from its Drivers directory after core memory/resource/file services are ready
and before launching Finder. Report misplaced package classes without executing
them; do not use cross-directory fallback discovery. Resolve declared dependencies
across both package sets in the shared registry. Define named service-ready phases
and allowed calls per phase; initialize shared registries once before concurrent
extension execution or secondary-hart startup.

Resolve a deterministic dependency graph: reject duplicate identities, cycles,
missing prerequisites and incompatible versions before entry. Do not use file
names as native dependency order. Stage registrations during fallible startup,
publish only on success, and undo staged ownership on a reported error. Rollback
covers only tracked operations; initialization must not perform untracked,
irreversible mutations. Failed providers leave dependants disabled with a reason.

Initially load only at boot and retain module memory until reboot. Changing the
enabled set affects the next boot. Live unload/replacement is a separate milestone
requiring callback quiescence, outstanding-client/device/work cancellation and
dependency teardown; calling a stop function alone is not enough. Define reverse
dependency shutdown and bounded flush behavior without claiming hot unloading.

Provide an extensions-disabled startup mode, reachable before optional module
entry, plus last-started/failed module diagnostics. Bootable core services cannot
depend on optional extensions needed to reach that recovery mode. Persist enabled
state as JSON; diagnostics must still work with read-only or damaged preferences.

An extension returning an initialization error is different from a kernel panic.
The current panic-abort build cannot unwind/recover a module. Plan a System-owned
fatal report containing extension identity, phase and available panic details,
with an emergency path that does not depend on the failed extension or held UI
locks. Never invoke the app-only crash/exit path or resume potentially corrupted
kernel state. Skip-startup recovery is not runtime panic containment.

## Legacy compatibility

Apple's [Control Panels chapter](https://dev.os9.ca/techpubs/mac/MoreToolbox/MoreToolbox-461.html)
defines the `cdev` callback lifecycle, storage, dialog and resource context.
Its example includes a panel with an extension. The
[Start Manager's extension contract](https://leopard-adc.pepas.com/documentation/mac/OSUtilities/OSUtilities-192.html)
describes `INIT` files/resources, including INITs inside Control Panels files.
Retain original System 7.6.1 probes for discovery, ordering, startup bypass,
resident-code lifetime and panel messages before claiming compatibility.

Existing `CodeRegistry` classification of `cdev`/`INIT` is only a translation
boundary. Legacy support needs executable translation, original entry/A5/global/
heap semantics, panel/dialog hosting and the selected trap/patch contracts.
Route supported legacy operations through resident services. Recognize hybrid
packages without executing their startup resource twice. Unsupported legacy
patches or services must be reported, not silently promoted to arbitrary native
kernel memory access. Native extension support does not imply original INITs
or control panels work unmodified.

## Delivery and acceptance

1. Define/package the native descriptor and SDK; add loader validation, module
   ownership, dependency planning, staged registration and startup bypass. A
   small boot extension publishes a typed query service, proving relocation and
   persistent state without entering the UI or polling per event.
2. Deliver preferences, thin management bindings and a resource-based companion
   control panel. Verify real setting changes, cancellation/error behavior,
   restart-required state and persisted JSON across reboot.
3. Add bounded hook/worker dispatch after the relevant scheduler foundations;
   verify contention, cancellation, reentrancy and actual thread context. Repeat
   on two/four harts when P1 is available. Add an Extensions Manager panel for
   enabled sets, dependency errors and next-boot status through the same APIs.
4. Capture original cdev/INIT behavior and implement legacy adapters in measured
   slices. Keep classic patch compatibility separate from the native hook API.

Canonical public-service assertions belong in `crates/mac-toolbox/tests` through
the SDK and real resident services. Kernel-only ABI/loader tests supplement them;
private QEMU boot fixtures exercise real extension entry and callback execution.
Cover independent relocation/data/BSS, malformed/unsupported modules, dependency
cycles/duplicates, init rollback, stale/foreign registration IDs, panel ownership,
separate Drivers/Extensions discovery, misplaced-package rejection before entry,
preference conflicts/write failures, disabled boot, boot-session pinning and
deterministic order. Run fatal-panic fixtures only in disposable test VMs and
verify a following extensions-disabled boot can reach Finder. Record startup,
dispatch and lock/interrupt durations; hooks must not add unnecessary per-input work,
polling, event storms or unbounded repainting. Preserve Cursor Test and existing
present/event-latency gates. No long callbacks become acceptable merely because
they originate in an extension.
