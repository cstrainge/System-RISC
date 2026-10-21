# Vhost shared-directory volumes

Status: implementation in progress, tracked as
[P17](CORE-TOOLBOX-ROADMAP.md#p17--vhost-shared-directory-volumes). The native HFS-B cutover and initial generic module foundation are verified. A shared host directory is optional runtime
storage; normal HFS-B boot must work when the host export is absent.

## Separate filesystem and block-device driver pair

Vhost has its own filesystem/block-device driver pair, using the
[kernel driver contracts](DRIVERS.md). Both drivers execute in the kernel and
require DRVR classification plus generated DABI contract metadata. This project
name does not select a particular QEMU protocol or imply that every host supports
the same transport.

- The vhost block-device/transport driver owns the VM-facing endpoint, request
  queues, transfer buffers, cancellation and negotiated host I/O capabilities.
  Its block interface serves actual block-backed exports. Directory exports need
  a distinct directory/file-operation contract over that transport, including
  enumeration, positional I/O, creation, rename, flush and change tokens.
- The vhost filesystem driver owns the guest catalog, fork-container decoding,
  Macintosh metadata, name/identity mapping and file/volume operations. It uses
  the device driver's checked services; apps never call the host endpoint.

Keep the pair independently contracted and dependency-ordered, with readiness
published only after both have initialized. A directory is not a disk image:
do not invent sector addresses for host paths or make the block-class interface
carry filesystem-specific metadata. Freeze the directory-service extension and
its dependency versions in DABI before implementing the pair. Common transport
code may be shared; neither driver duplicates HFS-B serialization.

Qualify the transport against the actual Linux and native Windows QEMU profiles
before selecting it. Prefer an existing suitable shared-filesystem transport
where available; document required capabilities and unsupported profiles. Keep
transport selection separate from the persistent fork-container format. Select
the transport after that qualification; it remains delivery work.

## One host file, two guest forks

Every guest file is stored as one versioned host file containing its data fork,
resource fork and Macintosh metadata. File Manager, Resource Manager, Finder,
Standard File and native/legacy applications see an ordinary dual-fork file.
Neither wrapper headers nor host filename encoding are visible in file content
or guest names. Apps use the same public Toolbox APIs on this and other volumes.

Define a byte-accurate format with magic/schema, checked 64-bit fork offsets and
lengths, NFC UTF-8 name, type/creator, Finder flags, required identity metadata
and unsigned 64-bit microsecond timestamps from the 1904 Mac epoch. Reuse the
native 255-byte post-NFC component and default Unicode folding rules. Do not
inherit classic MacBinary name/date limits or require per-file resource sidecars,
host extended attributes or alternate data streams. Keep this format distinct
from the existing build-package MacBinary transport and the HFS-B disk format.

Host folders remain directories. Specify root/directory metadata and reserved
internal records separately; these must not appear as guest documents. Freeze
a reversible portable host-name encoding that handles Windows reserved names,
characters, trailing dots/spaces, case and normalization differences. Guest names
remain case-preserving NFC and case-insensitive regardless of host behavior.
Externally introduced name collisions must report a conflict, never merge or
overwrite files. Invalid/unsupported containers report explicit diagnostics;
ordinary unwrapped host files require an explicit import policy/tool rather than
being silently rewritten.

Share the safe no_std format codec (`mac-fork-file`) between the driver,
Linux init/import/export/check tools and the future System RISC Filesystem app.
Reuse common name/date/metadata primitives rather than copying implementations.
Tools must wrap/unwrap both forks losslessly and validate without modifying files.
The native utility uses public Toolbox services for mounted storage.

The shared codec and `tools/risc-forks` init/import/export/check utility are now
implemented. The [container layout](../FORMAT.md#native-host-fork-containers-rsfork01-version-1)
fixes offsets, identities, Unicode, integrity checks and staged publication.
Tests exchange both forks and native metadata with HFS-B. Bulk import holds the
writer lock and updates a cached directory index after each publication. This
host-tool milestone does not yet provide a guest mount or filesystem driver;
the read/write guest acceptance below remains required.

## Identity, changes and persistence

Persistent file identity must survive rename/move and atomic replacement used by
the driver. Define copied-container identity collision handling before freezing
the format; embedded IDs copied on the host cannot make two files one object.
Host path strings and recycled host inode/file numbers are insufficient runtime
IDs. Mount/node/fork/request handles use the shared generation-tagged facility,
with separate owner checks and invalidation on eject or transport reset.

Negotiate read-only/read-write, atomic replacement, flush and notification
capabilities. Write both forks and metadata as one logical container revision,
with staged commit and explicit recovery/error behavior. Claim durability only
to the extent the host transport actually guarantees it; include directory
publication/flush where supported. Do not report successful durable writes on an
unsupported backend. Unsupported write prerequisites require a read-only mount
or an explicit refusal, not an undocumented weaker contract.

Open leases and revision tokens prevent reads mixing forks from different host
revisions. Detect external modifications; conflicting writes return a defined
conflict instead of overwriting host changes. Use bounded change notifications
or background polling, cache unchanged catalogs/container headers, and invalidate
only changed objects. Host scanning and container decoding must not run per mouse
event or block the UI under File Manager locks.

Constrain operations to the explicitly exported root, including symlink/reparse
resolution and rename races. Native apps receive checked file capabilities, not
arbitrary host paths or host process privileges. Removing the export cancels I/O
and reports unavailable storage; it cannot leave callbacks targeting reused IDs.

## Acceptance

Public SDK tests in `crates/mac-toolbox/tests` cover ordinary file/resource access,
both-fork writes and readback, metadata and precise dates, empty forks, large
offsets, Unicode/case collisions, portable names, rename/move/copy identity,
read-only behavior, eject, missing exports and stale/foreign IDs after slot reuse.
Linux tools produce fixtures, the guest modifies them, and Linux checks exact
fork bytes and metadata again. Exercise Resource Manager/ResEdit and copy between
HFS-B, StuffIt volumes and host volumes through the same APIs.

Test malformed containers, external edits during open/write, copied IDs,
interrupted commits, transport/flush failures, root-escape attempts and concurrent
apps. Qualify the actual selected transport on Linux and native Windows QEMU;
codec-only success is not a working shared volume. Record bounded queue/cache
behavior and Cursor Test/present timings while host activity occurs. No host
volume is a prerequisite for the HFS-B-only boot acceptance gate.


## Transport qualification

The initial host bridge uses a virtio-serial device with a port 0 virtconsole byte
channel, backed by a loopback TCP host companion. This is an opaque protocol
channel, separate from the application's terminal UI and the diagnostic UART.
Only explicitly exported roots are exposed; no workspace export is implicit.
Directory and actual block exports use different negotiated class protocols over
this channel. The persistent fork-container format does not depend on QEMU.

A disposable RV64 hardware probe exchanged `GUEST_RPC_PING`/`HOST_RPC_PONG` through
real virtqueues on both installed Linux QEMU and native Windows QEMU 10.1.0.
The actual Windows binary has no 9p or virtio-fs device, while both profiles expose
virtio-serial/virtconsole. Evidence fingerprints and command information are in
the private `results/system7/host-bridge-qualification.json` report; private probe source/output remain
outside the OS repository. This qualifies channel delivery only. It does not
qualify the filesystem protocol, native Windows filesystem persistence, shared
volume read/write behavior, notifications, or disconnect/cancellation handling;
those are mandatory provider gates before declaring the feature complete.

## Host filesystem implementation checkpoint

`drivers/vhost-filesystem` now contains the safe `no_std` asynchronous catalog and
fork-container client. It reads leased fork snapshots, caches normalized names
and folded lookup keys, scans one child per step, detects external changes, and
creates/replaces both forks through durable staged host publication. Writer
leases and reserved catalog IDs survive the required retry/abort paths; metadata
cleanup errors cannot cause a second publication. Own writes update the affected
folder cache rather than re-enumerating unrelated directories.

Ten real-host component tests cover these paths, including read-only refusal,
Unicode collisions, stale tokens, corrupt data, external refresh, flush/close
failure and abandoned acquisition cleanup. Strict host and RV64 library checks
pass. These tests do not mount a guest volume: the native module wrapper, generic
filesystem class, File Manager integration, remaining catalog mutations and the
public-SDK VM read/write gate are still pending.
