# mac-hfs-b

Shared safe `no_std` HFS-B version-one filesystem, with explicit per-operation
allocation/work budgets and caller-owned sector storage. The crate implements
format, mount/check, catalog creation/enumeration, rename/move/delete, metadata
and comments, blessing, and full random-access/streaming read/write/resize for
both data and resource forks. File lengths/offsets are 64-bit. Linux adapters,
System and ROM share these algorithms; none belongs in a Python disk builder.

The serialized contract is [HFS-B-FORMAT.md](../../docs/HFS-B-FORMAT.md). Version
1 implies Unicode 17.0.0, NFC storage, canonical caseless comparison and unsigned
1904-epoch microseconds. Versions are whole integers, not semantic versions.
Updating Unicode requires a new whole filesystem contract and rebuilding all
affected name/file/index representations. Unsupported media fails explicitly.

`Budget` bounds work per operation, not total volume/fork size. Stream large
I/O in bounded calls; seek gaps and growth are zero-filled. Writers use fresh
payload blocks and bounded metadata undo. Successful calls flush durable commit;
reported failure either preserves the previous reachable state or reports failed
rollback/dirty media. There is no journal, repair tool or power-loss atomicity
claim. Dirty/mismatched/incomplete headers are rejected. `check` is a separate
full-volume operation and needs a caller-supplied working-memory budget.

Linux commands are supplied by `cargo run -p hfs-b -- help`. For example:

```sh
cargo run -p hfs-b -- create ../build/system7/example.hfsb 524288000 'System RISC'
cargo run -p hfs-b -- mkdir ../build/system7/example.hfsb 2 'System Folder'
cargo run -p hfs-b -- list ../build/system7/example.hfsb
cargo run -p hfs-b -- check ../build/system7/example.hfsb
```

`format` accepts an explicit image/unmounted block-device range. Writable Linux
block devices use O_EXCL; regular image adapters take an exclusive file lock.
`import` preserves MacBinary's forks and representable metadata; `put` streams
arbitrarily large native forks. `export`/`restore` use data/resources files and
versioned JSON metadata to preserve native UTF-8 spelling, full timestamps,
FinderInfo, flags and comments. A failed multi-call import can leave a partial
file; these tooling operations do not claim atomic whole-document publication.

Verification:

```sh
cargo test -p mac-hfs-b -p hfs-b -p mac-hfs
cargo test --release -p mac-hfs-b --test large_forks -- --ignored
python3 scripts/generate_hfs_b_unicode.py
HFS_B_NORMALIZATION_TEST="$PWD/../build/system7/unicode-17.0.0/NormalizationTest.txt" \
  cargo test -p mac-hfs-b --test unicode -- --ignored
cargo check -p mac-hfs-b --target riscv64imac-unknown-none-elf
```

The tests cover 20,034 official Unicode normalization rows, name/collision and
date boundaries, growing/fragmented trees/forks, actual allocated writes beyond
4 GiB using a sparse storage adapter, real Linux image/remount and >4 MiB fork
round trips, write/flush/undo failures, read-only media and serialized vectors.
Canonical resident/public-SDK and actual ROM/Finder cutover gates remain required
in P15; host-only tests do not establish those capabilities.
