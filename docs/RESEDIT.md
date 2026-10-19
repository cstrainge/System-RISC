# ResEdit

Phase 1 installs `Applications/ResEdit`, a read-only resource browser with native
lossless JSON export. It uses owned Toolbox windows, the System-owned panic
reporter, and resource-based menus, labels and layout. Original ResEdit 2.1.3
large/small monochrome, 4-bit and 8-bit icon families, BNDL and FREF resources are
copied byte-for-byte; hashes and Apple's download URL are recorded in
`apps/resedit/assets/provenance.json`.

File > Open (Ctrl-O) uses the shared Standard File Package. Double-click a
folder, or select it and press Return; Command-Up (Ctrl-Up on our keyboard)
goes to its parent. The directory popup lists ancestors, Desktop lists mounted
volumes, and Command-Left/Right cycles volumes. Typing selects a file by name.
Escape or Command-period cancels. The Open API accepts `TypeFilter::All` or a
list of up to 16 four-byte file types. Folders remain navigable. As documented
for StandardGetFile, filtered lists omit invisible items, while -1/all types
includes them. ResEdit opens all types because resource forks are not limited
to applications.

Select a resource type in the left column, then a resource in the next. Both
columns have independent Prev/Next paging. The information panel shows type,
signed ID, name, exact byte length and attributes. Previews decode MENU headers
and items (enable/mark/key/style/icon fields), STR/STR# MacRoman strings, and
vers numbers, stage, country and strings. ICON/ICN#/cicn use resident Icon
Utilities. CODE, RV64 and unknown binary resources use paged hex. Page Up/Down
scrolls the preview; Home returns to its start. Structured/icon previews have a
32 KiB display limit; larger resources remain available as hex and export
completely. Inspection never loads executable resources.

`Resources::open_file(volume, cnid)` returns an owned read-only `ResourceFile`.
Enumeration, lookup, metadata and partial reads borrow an owner-checked File
Manager resource-fork mark. No extra object IDs or resource-chain scopes are
created. Drop closes the fork. Independent inspectors and a running application
may read the same file.

## Save, Save As and rebuild

File > Save JSON (Ctrl-S) prompts for a destination on its first use and then
saves to that destination. Save JSON As always presents StandardPutFile, with
the current name and directory. Export JSON (Ctrl-E) uses the same native save
path. System owns both dialogs, filename editing, keyboard/mouse events, folder
navigation, New Folder, error reporting and replacement confirmation. Saving
creates an ordinary TEXT data file; it never edits the inspected resource fork.
The original DLOG/DITL resources -6042 through -6046 own the Open, Save, error,
Replace and New Folder geometry. The Rust facade carries UTF8 strings and
FSSpec-style catalog selections. Pascal/MacRoman conversion exists only in the
companion legacy traps, using the same owner/generation-checked resident state.
The classic 68k Pack3 adapter is outside this change.

A `Files::create_data(DataFileSpec, length)` writer stages up to 4 MiB fallibly in
System memory. Drop aborts; `finish` requires exactly the declared length and
publishes the file only after the data reaches free HFS blocks and is flushed.
Replacement requires the confirmed catalog ID, preserves that identity and the
resource fork, and rejects open/locked/protected files. Metadata writes use the
existing rollback transaction. Reported write/flush failures preserve the old
live file; failed metadata rollback poisons the volume. This is not power-loss
atomicity, a general resource-fork writer, catalog-fork growth or a Rust std
filesystem implementation. Capacity/allocation limits return errors.

The saved `resources.json` uses the existing schema's `archive: true` mode:

```sh
cargo xtask rebuild /path/to/resources.json /path/to/rebuilt
```

Every resource uses raw hex, preserving FourCC bytes, signed IDs, absent versus
empty names, all attribute bits and payloads. Both forks and file type, creator,
Finder flags and creation/modification dates survive. mac-build never regenerates
RV64/NABI/RLOC/vers in archive mode; a rebuild needs no Cargo project or compiler.
Ordinary application manifests still generate versions from Cargo. Legal HFS
slashes/backslashes stay in the Macintosh header; rebuild escapes their host
output filename so it stays inside the chosen output directory. Resource-map
padding/order and HFS/Finder catalog bookkeeping are not part of the archive.

File > Export JSON to Host is an optional serial transport. After selecting it:

```sh
python3 scripts/export_resedit_json.py --output /path/to/resources.json
```

The helper validates sequence, length, FNV64 and complete JSON, and refuses to
overwrite a host file. `--serial` and `--token` select a different captured export.

The current Standard File profile supports mounted fixed disks, 4096 direct
entries per directory and no app callbacks. Eject is disabled for these disks.
Alias resolution and the original custom hook/filter callbacks require later
Alias Manager/custom-dialog work; unresolved aliases return an explicit error.
The initial slice does not claim full Standard File Package compatibility.

## Evidence and acceptance

Original behavior comes from Inside Macintosh: Files, Standard File Package,
[StandardGetFile](https://dev.os9.ca/techpubs/mac/Files/Files-307.html),
[StandardFileReply](https://dev.os9.ca/techpubs/mac/Files/Files-306.html) and
[dialog resources and navigation](https://dev.os9.ca/techpubs/mac/Files/Files-308.html).
The Retro68 fixture in `crates/mac-toolbox/tests/fixtures/standard-file-original`
was executed on a private System 7.6.1 Quadra guest. Reference frames, outcomes,
resource bytes and hashes are under `tests/data/standard-file`.

Canonical checks live in `crates/mac-toolbox/tests`, through the aggregate SDK:

```sh
cargo test -p mac-toolbox --test resedit --test file_mutations --test file_ownership
cargo xtask image
cargo test -p mac-toolbox --test resedit -- --ignored
cargo xtask check
```

Coverage includes all resource fields and both forks, malformed maps, invalid
code that must never execute, unusual type/name bytes, payloads over 1 MiB,
original icon hashes, actual icon pixels, native saves extracted from a stopped
private QEMU disk, archive rebuild, running-app inspection, filters, modal
cancellation/replacement, New Folder, stale/foreign IDs, ownership, legacy string
conversion, remounts, and injected write/flush failures. Python supplies transport;
Rust makes acceptance decisions. The visible desktop is not used for test input.

Phase 2 adds RV64 disassembly and NABI/RLOC loader views. Phase 3 adds editing
only when Resource Manager writing exists. No resource-fork write path is added
by this application.
