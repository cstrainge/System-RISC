# Resource-based Finder menus and Trash

Finder links only `mac-toolbox`. The SDK marshals calls; System owns manager
state and device access. Unique menu geometry, drawing and hit code lives in the
independently loaded MDEF0 resource alongside WDEF0 and CDEF0/1.

Uncached sizing is checked against eight executed original CalcMenuSize cases.
Standard MDEF reserves24 pixels beyond the label for keyless items and48 for
items with shortcuts. The latter preserves a10-pixel gap between the label and
command column. This makes Finder's generated File menu139 pixels wide, matching
the original sizing operation; the former127-pixel result overlapped Close Window
with Command-W. Host and RV64 public-API tests share the captured sizes, with
additional complete-label/gap/shortcut pixel checks.

## Resource ownership

Finder's MENU1251..1256 preserve the original English Finder7.6.1 fmnu command
identities and shortcuts. The original fmnu resources are retained unchanged.
MENU/MCMD are resolved native resources: placeholders for About, Print Window,
Shut Down and dynamic View/Label names are expanded at packaging time. About
uses this OS's System RISC branding. Unsupported commands remain disabled.
The structured menu compiler accepts text/key/mark/style/enabled/checked/command
fields; command fields produce a same-ID MCMD companion automatically.

New native interface/error strings live in STR#2000. Trash caption, default
folder name and About text are loaded through Resource Manager UTF8 string APIs.
Empty Trash reads the original STR#11750 item6 and interprets its singular/plural
and count/allocated-space substitutions. ALRT6012/DITL6012 supply the original
confirmation rectangle, buttons, caution icon and ^0 body. ALRT/DITL2001 are a
one-button error adaptation of that original template. Dialog Manager invokes
WDEF1 and CDEF0, derives alert placement from the WDEF structure and the desktop's
upper-fifth rule, and routes input modally. Enter confirms and Escape cancels;
button highlighting follows press, leave, reentry and release.

Reference resources and actual System7.6.1 menu/Trash captures are recorded in
`../results/finder-menu-trash-reference` in the enclosing RE workspace. Source
Finder MacBinary SHA256:
`647b19dba89dc0167ae45dda20fd02bd8e44c91252e7513c9c584f1588eb4d3f`.
Inside Macintosh documents the [ALRT format](https://dev.os9.ca/techpubs/mac/Toolbox/Toolbox-437.html)
and [DITL format](https://dev.os9.ca/techpubs/mac/Toolbox/Toolbox-438.html).

## Storage and identity

New Folder, move to Trash, Put Away, usage queries and recursive Empty Trash use
real HFS catalog records. Per-volume root Trash directories use the original
name-lock flag0x1000 and retain the item's original parent in extended Finder
information. Restoration preserves CNID and both fork contents; the recorded
Put Away parent persists, matching the original observation. Finder hides these
special root directories from ordinary folder views and combines their children
in one virtual Trash window. Selection carries volume plus CNID.

The bounded writer operates within existing catalog/extents-tree reservations,
updates index/thread/map nodes, directory counts, MDB copies and allocation bits,
and explicitly flushes virtio storage. It preflights edits, keeps sector undo
records and rolls back reported write/flush failures. Failed rollback disables
further writes until remount. Protected boot files, open files, readonly media,
name collisions and unsupported capacities fail explicitly. This does not
provide power-failure atomicity or an on-disk recovery journal. General fork
writes, allocation growth, rename and JSON preference transactions remain future
work; the native std filesystem port still needs those services.

Each mutation accepts a native Timestamp from the wall-clock service and checks
classic1904 epoch conversion. The RISC-V machine adapter reads Goldfish RTC at
0x00101000, low word before latched high word, under a short shared lock. See
QEMU's [virt board](https://github.com/qemu/qemu/blob/master/hw/riscv/virt.c) and
[RTC implementation](https://github.com/qemu/qemu/blob/master/hw/rtc/goldfish_rtc.c).

## Verification and limits

Canonical assertions live under `crates/mac-toolbox/tests`: original menu
metadata, real userspace menu events, malformed resource errors, native/legacy
string conversion, native clock and actual writable HFS persistence/rollback.
Capture scripts operate only on private clones. Existing graphics, windows,
controls, fonts, icons, PNG, boot and boundary checks remain part of the gate.

Only the supported ALRT profile is implemented: DLOG, edit/user items and alert
stages are future work. Original MenuSelect/classic WindowRecord contracts and
68k execution are not supplied by these native interfaces. Apple accessory
icons and colored Label-menu samples are not yet complete original rendering.
Menu resource handling and a passing bounded comparison do not establish the
entire classic Menu/Dialog/File Manager API.
