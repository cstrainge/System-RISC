# Finder Get Info

This implementation targets ordinary files and folders from Finder 7.6.1 on
System 7.6.1. Editable names are an intentional native extension requested for
RISCy System. Original Finder's name view remained static during the recorded
click, double-click, Select All and typing observations.

## Reference and evidence boundary

The original Finder MacBinary SHA256 is
`647b19dba89dc0167ae45dda20fd02bd8e44c91252e7513c9c584f1588eb4d3f`.
Its Finder version resource reports 7.5.6. The immutable reference is outside this
repository at `working/finder-reference/Finder.bin`. The resource fork starts at
128 and is 515019 bytes. The analysis exports 228 decompressed CODE segments;
original and derived hashes and offset 4 slice provenance are retained alongside
the REA dossiers. Analysis addresses match original CODE resource offsets.

Runtime observations, original screenshots, WindowRecord measurements and
hashes are in `results/finder-get-info-reference/RUNTIME-FINDINGS.md` outside
this repository. The isolated original disk was restarted to verify persistence,
then deleted. The original baseline disk and installation media are retained.
Resource inventory and extraction provenance are in
`analysis/finder-get-info-resources.md` and the bundled asset provenance file.

| Finding | Evidence | Coverage |
| --- | --- | --- |
| File menu command is `sinf`, Command-I; layouts are custom `fvew`, not DLOG/DITL | MENU1252, fmap5011, STR3091, nine decoded `fvew` variants | Direct serialized resources |
| File/document content is242x261; folder is242x217; text uses application font1 at 9 points | `fvew`12761/17011/11261 and original frames | Resource coordinates corroborated at runtime |
| Folder totals include descendant folders and files; size distinguishes logical and allocated fork lengths | Empty and nested-folder original captures | Recorded fixtures, not a universal formatting oracle |
| Comments accept CR line breaks and survive reopening and cold restart | Original folder/file edit sequence and decoded Desktop DB | Observed runtime and disk bytes |
| Locked disables comments/Stationery; Stationery updates the icon without changing the displayed dates/kind | Original checkbox and reopen captures | Recorded ordinary TEXT file |
| Reopening the same object's Info reuses its moved window | Recorded WindowRecord identities | Observed original runtime |
| Locked/name-view command dispatch is in CODE130:0x5ca | REA `ev_d9de602429e36f22a179e788fef346ea6fe714aa99582712ddfd6eb066d539d8` | Complete 342-byte dispatch routine; indirect targets remain inferred |
| `info` dispatch calls A5+0x30aa, resolved through decompressed CODE0 to CODE74:0x470 | Same dispatch plus REA `ev_b96353d522e8b688d38a550f499f0842f340350c5d7027ca50887e0110634a64` | Complete 48-byte wrapper, dispatches indirectly through vtable+0x1b0 |
| CODE74:0x444 prepares the information request through vtable+0x1b8 | REA `ev_90d66970e741c7e2f05a73b077fd0080cf849f8445776cdf238b420f18715104` | Complete 44-byte wrapper; concrete constructor not recovered |
| Comment getter uses a 255-byte request, selector0x2a/A260; alternate path references FCMT | CODE87:0x442, REA `ev_a5c50686d13daa72c8b127be94951c8325695163f28a10ddc30a5c5de8114ba8` | Ghidra truncates at A-line traps; raw bytes at 0x484 are702a a260 |
| Comment setter passes Pascal payload length, selector0x28/A260; alternate path references FCMT | CODE172:0x352, REA `ev_283ad19e21dd53f05488d311c65525373d80d6bca7e63f795c0c91336fa65711` | Partial noncontiguous decompilation; raw bytes at0x39e are7028 a260 |
| CODE132 routines include property formatting/child-view handling | REA `ev_64c7199952be32f0850631ee6b81cfa2e05b831d6976954f0e9cf063421c22de`, `ev_1d891c5145818858a5f612a063ee2841f5d0f09e84dac273ef61353e3f6de0f1` | Does not establish a concrete Info constructor |

REA pseudocode is recovered analysis, not original Apple source. The complete
bundles and provider-neutral snapshots live in `working/finder-get-info-static`.
Ghidra 12.1.2, raw m68k BE 68000 profile with offset0 mapped at 0x4 was used.
A-line trap truncation is retained as a limitation, not silently treated as an
end of routine. Application memory fields, aliases, icon-copy Scrap payloads and
volume Info were observed/inventoried only where explicitly recorded; these
are not claimed as implemented by the ordinary-file/folder controller.

## Native implementation

Finder owns each modeless Info controller and its editors, keyed by(volume,CNID).
The public Toolbox owns TextEdit, resource reading, windows, controls and disk
operations. Original `fvew` and string resources accompany a checked native
layout projection. Definition code remains in CDEF/WDEF resources.

Comments initially receive focus. Name editing commits on Return or focus change;
comments commit on focus change or close. Successful rename retains CNID and
forks. Invalid names and write failures preserve the draft and show an error.
Info opts into deferred close-box requests, so the application commits before
explicit Window Manager disposal. Other applications retain automatic close.
Editors are disposed with their controller, including partial-open failure paths.

Disk-backed `vers` reads use a size-distinct Resource Manager request. The service
reads a bounded resource map and at most 518 bytes of selected version data;
it does not load executable resources or alter the current resource chain.
Classic disk strings are decoded to UTF8; native clients expose Rust strings,
with a separate Pascal output conversion at the legacy trap boundary.

Folder totals use a revision-bound directory cursor. Each native scan reads at
most one leaf and examines at most eight records. Finder yields between small
steps, restarts on catalog changes, and reports excessive nesting/cycles or
repeated mutation explicitly. A 64-level traversal bound is not silent truncation.
The HFS descriptor is prepared during mount, avoiding repeated cold mounts on
every scan step. Existing index-based directory APIs remain available.

Desktop DB comments are type 3 records with a six-byte key (length 5, kind 3,
big-endian CNID), followed by a Pascal MacRoman value. Missing comments are empty.
The limit is 255 MacRoman bytes; this is the observed storage/request bound,
not a claim that every original UI accepts255 characters. New Desktop DB files
are invisible BTFL/DMGR files with a 64 KiB data fork. Updates preserve unknown
leaf records and header user data. Allocation bitmap, data sectors, catalog and
MDB changes share the existing rollback transaction. Reported write/flush
failure rolls back; irrecoverable rollback poisons the mount. This is not a
power-loss journal. Unchanged setters do not write.

## Remaining compatibility work

- APPL memory requirements, alias/volume/Trash-specific Info variants.
- Desktop Manager kind descriptions and custom icon editing/copy formats.
- FCMT fallback for volumes without the modern Desktop DB convention.
- Desktop DB growth after reserved capacity is exhausted; current service reports
  DISK_FULL. Desktop DF and unrelated record semantics remain opaque.
- Less conservative handling of protected/open objects and volume renaming.
- Locale-specific date/number formatting beyond the current resource labels.

Compatibility assertions belong in `crates/mac-toolbox/tests`: persistent file
mutations/comments, original Desktop DB preservation, bounded directory scans,
file-backed versions, font-aware TextEdit, native/legacy string conversion and
isolated graphical input/capture. Capture scripts collect evidence; Rust owns
acceptance decisions. See each test/report for actual executed coverage.

The control definition also accepts the original application-font alias and
Geneva 9 bold captions. Existing Chicago/Geneva regular geometry remains on its
original path; bold ink uses resident Font Manager drawing with a saved and
restored surface clip. Dedicated SDK tests cover complete controls, partial
glyph clipping, and clip restoration after service errors.

## Executed Get Info acceptance

The actual Finder capture passed 20 input stages across two cold boots on both
Linux QEMU and native Windows QEMU. It verifies stable-CNID file/folder renaming,
window reuse and moved position, changed title/browser caption pixels, multiline
comment persistence, Locked/Stationery behavior, and retained drafts/windows on
duplicate-name failures. Public SDK reads independently check the resulting HFS
names, comments, flags, dates and payload. Five original file/folder UI states
match exactly over 38,891 compared pixels (fixed labels, name, checkbox rows and
comment borders). Dynamic kind/path/date/icon differences are explicitly outside
that pixel count. The original screenshots and their hashes are frozen in
`crates/mac-toolbox/tests/data/get-info`.

Reports outside the repository are `results/system7/get-info-ui-checks.json`
and `results/system7/get-info-ui-windows-checks.json`. Private VM disks and raw
inspection snapshots are deleted after acceptance, including failures; frames,
serial logs and media hashes remain. Graphical I/O fault injection is not
covered; separate public SDK backend tests exercise write/flush failures and
rollback. This scope does not claim all Info variants or all Toolbox services.
