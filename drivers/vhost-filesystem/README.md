# Host fork-container filesystem provider

The safe, `no_std` library implements host container catalogs, leased reads and
staged two-fork creation/replacement over the separate `HDIR` transport contract.
Names, identities and integrity use `mac-fork-file`. Native files and folders
use their actual readable names on the host; the host directory entry supplies
the current name while embedded identities survive renames and remounts. Guest
creation validates portable names before reserving IDs or publishing anything. Ordinary host files and folders also enumerate
without conversion: file bytes form the data fork, the resource fork is empty,
and names/dates are projected from the host. No metadata sidecars are created.
The companion pins these host objects and supplies session/export-scoped
generation-tagged catalog identities, preserving rename identity and rejecting
replacement reuse. Runtime volume/open/write tokens come from the resident generation
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
New directories publish their native metadata record atomically with the directory;
they may be created inside either native or ordinary host folders.

`tests/reader.rs` runs against real host directories and the production host
companion session, with split future polls, both forks, Unicode/case collisions,
external changes, stale/foreign tokens, flush/close failures, cancellation and
ID reservation. These are component tests; native public-SDK guest coverage is
documented in `docs/FILESYSTEM-INTERFACE.md`. The normal launcher exposes a default
Transfer volume plus explicitly selected exports. System mounts them before
launching Finder. Provider app loading, remaining mutations, complete Finder
copying and external-change refresh are still delivery work.

The common `FSYS` v2 byte codec and its library-side service dispatch now exist;
see `docs/FILESYSTEM-INTERFACE.md`. `DirectoryRpc` adapts the checked module
provider capability into bounded futures with reusable packet buffers. Its ticket
survives an abandoned future, and draining returns an acknowledged host mutation
instead of pretending cancellation undid it. The native Host Filesystem module
and its separate Host Transport dependency are installed as `DRVR` files in
`System Folder/Drivers` by the normal image build. Discovery skips them when the
optional virtual-host channel is absent.
