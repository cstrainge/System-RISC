# Host fork-container filesystem provider

The safe, `no_std` library implements host container catalogs, leased reads and
staged two-fork creation/replacement over the separate `HDIR` transport contract.
Names, identities and integrity use `mac-fork-file`; host paths never become guest
filenames. Runtime volume/open/write tokens come from the resident generation
facility; the driver does not issue a parallel set of object IDs.

Every transport await retains owned progress, and catalog/chunk loops yield
between bounded steps. No File Manager lock may be held while polling these
operations. The native adapter must drain/cancel a pending lower RPC before
releasing its operation future. Acquired capabilities are tracked until close or
commit acknowledgement; idle cleanup releases acquisitions abandoned before a
catalog, open lease or write transaction took ownership.

Writes reserve catalog identities durably before file publication, retain root
writer leases through publication, flush chunk bytes and the final valid header,
and preserve publication state for retries after cleanup failures. Abort reports
whether publication already happened; it does not claim to undo a committed file.
A higher-level cancellable batch transfer must handle rollback of its outputs.

`tests/reader.rs` runs against real host directories and the production host
companion session, with split future polls, both forks, Unicode/case collisions,
external changes, stale/foreign tokens, flush/close failures, cancellation and
ID reservation. This is component coverage. The library is **not yet a loadable
module or a mounted guest filesystem**; File Manager routing, the native wrapper,
rename/directory/delete operations and public-SDK VM acceptance remain delivery
work. Host-directory mounting is not ready for interactive use yet.

The common `FSYS` v1 byte codec and its library-side service dispatch now exist;
see `docs/FILESYSTEM-INTERFACE.md`. `DirectoryRpc` adapts the checked module
provider capability into bounded futures with reusable packet buffers. Its ticket
survives an abandoned future, and draining returns an acknowledged host mutation
instead of pretending cancellation undid it. These remain component contracts,
not an installed native module or public File Manager mount.
