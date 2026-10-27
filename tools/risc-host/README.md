# Host directory companion

`risc-host` serves explicitly selected directory capabilities over the VM's
loopback byte channel. It does not interpret fork containers or synthesize disk
sectors. The guest filesystem driver owns Macintosh catalog/fork semantics.

```sh
cargo run -p risc-host -- --export 'External Software=/tmp/mac-share'
# Prints HOST_BRIDGE_READY 127.0.0.1:PORT for the VM launcher.
```

Use `risc-forks init/import` to prepare the native directory layout first. No
workspace directory is exported implicitly. `--read-only-export NAME=PATH` opens
a read-only export. `--listen 127.0.0.1:PORT` selects a fixed loopback endpoint.
A companion serves one VM connection at a time; a reconnect gets fresh IDs and
drops old locks, snapshots, enumerations and unpublished transactions.
`--one-session` serves one connection and exits when the VM disconnects. It also
exits if no VM connects within 30 seconds. The normal launcher uses this mode,
exposes its prepared Transfer folder by default, and accepts additional explicit
exports; it never exports an arbitrary workspace directory.

Read/write exports support Linux and native Windows host adapters. Startup
probes staged creation, file sync, no-replace publication/move, replacement and
directory sync, and refuses a write claim when any prerequisite fails. On Windows,
directory flushes use a write-capable handle to the same verified pinned object;
file and directory flush failures propagate rather than acknowledging a save.
Windows requires Windows 11 24H2 or newer and the workspace's nightly Rust
toolchain (including the `windows_by_handle` and `rename_noreplace` APIs).
NTFS and ReFS must pass the same admission probes. The macOS adapter remains
unverified on a real Mac; other unsupported hosts fail closed.

Writes target unpublished staged files. Commit flushes the file, publishes it,
then flushes the directory before acknowledging success. A failed commit keeps
its transaction handle for retry/cancellation; a publication followed by a
flush error is reported as an error. Repeated commit after publication completes
the remaining flush, rather than publishing a second object. Moves use Linux
`RENAME_NOREPLACE`, preventing an external destination race from overwriting
another object. Windows uses the standard library's atomic no-replace rename.
File positions and sparse lengths are checked 64-bit values.

Windows notifications share one recursive, overlapped `ReadDirectoryChangesW`
request across an export's physical directory subscriptions. Each revision check
processes at most one 4 KiB batch; overflow invalidates affected views. Entry
metadata filters duplicate notifications without opening data handles. A moved
subtree continues to use its pinned directories; leaving the recursive watch
invalidates the tracking gap and arms a new subtree watch. Shutdown cancels and
waits for pending I/O before releasing buffers.

Volume serial number, the full 128-bit file ID, and creation time identify
Windows objects. Change time and link count also participate in revision checks,
so restored modification times cannot hide ordinary edits. Directory watches
route by physical ID even when a host application edits a directory's creation
date. Missing handle metadata is an error, never a fallback identity.

Directory capabilities are opened by file ID. Cached file metadata uses retained,
already unlinked hard links outside movable folders, shared across sessions.
Creating those pins takes a short nonblocking data lock and validates identity;
it does not silently conceal concurrent content changes. The reserved
`.system-risc-pins` directory is hidden from guest enumeration. Startup recovers
only verified aliases that have another surviving link; a lone copy or foreign
substitution fails admission with instructions to preserve recovery records.

Close ordinary document snapshots before moving their parent folder on Windows;
otherwise the move fails and the owned intent remains available for cancellation
or retry. Cached metadata pins do not impose this restriction. For its private
removal record, the service journals the identity before temporarily closing
the record handle, moves the pinned folder, and reopens and checks the record.
A failed or substituted re-pin keeps its expected identity for recovery and
cannot authorize deletion. Owned publication/removal uses POSIX unlink semantics
so an acknowledged namespace change is complete even while an older snapshot
still pins the object. These operations retain the file/directory flush gate.

Snapshots and enumerations check host revision tokens. Replacement/removal
requires a matching open source capability; stale objects and conflicting
revisions fail. The guest filesystem must hold the export's cooperating writer
lock across catalog transactions, including root-counter updates. These locks
coordinate System RISC writers and tools; they do not isolate an arbitrary host
program which ignores the lock and edits the directory concurrently. Revision
checks report observed external changes; they are not a host filesystem CAS
primitive or an authentication signature.

The shared wire codec is `mac-host-protocol`. Requests are bounded to 64 KiB,
framed explicitly, and address children through directory capabilities plus one
checked component. Symlink entries and traversal paths are rejected. All live
IDs use `mac-object-table`, with separate session-owner checks.

Host tests cover real filesystem operations, shared pins, external subtree moves,
recovery refusal/retry, and a fragmented TCP exchange. The Windows-built System
and public SDK have passed guest captures under Linux QEMU and native Windows
QEMU's GTK frontend against native Windows companions on ReFS: both forks,
recursive transfers, cancellation, Trash/Put Away,
application-exit cleanup, external-change watches, and cold-boot metadata restore.
The prepared-capture Rust acceptance test checks the same assertions as the
regular guest harness. This does not establish physical power-loss behavior,
exhaustive Finder gesture coverage, or full original Macintosh compatibility.
