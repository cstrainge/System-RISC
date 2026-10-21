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

Read/write exports currently require the qualified Linux host adapter. Startup
probes staged creation, file sync, no-replace publication/move, replacement and
directory sync, and refuses a write claim when any prerequisite fails. Native
Windows QEMU can use this Linux companion; native Windows host-filesystem write
support is not claimed. Unsupported native host platforms can serve read-only
exports.

Writes target unpublished staged files. Commit flushes the file, publishes it,
then flushes the directory before acknowledging success. A failed commit keeps
its transaction handle for retry/cancellation; a publication followed by a
flush error is reported as an error. Repeated commit after publication completes
the remaining flush, rather than publishing a second object. Moves use Linux
`RENAME_NOREPLACE`, preventing an external destination race from overwriting
another object. File positions and sparse lengths are checked 64-bit values.

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

Host tests cover real filesystem operations and a real fragmented TCP exchange.
This is the companion milestone, not evidence that a guest volume is mounted.
Guest provider, File Manager and public-SDK acceptance remain required.
