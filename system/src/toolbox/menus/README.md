# Resident menus and loaded MDEF

The public `System::menus()` client carries requests only. The resident manager
owns menu objects, enable/check state and pointer capture; independently loaded
`MDEF` resource0 owns title/popup layout, drawing and item hit testing. Definitions
draw through resident Surface, Font and resource services rather than embedding a
second graphics engine.

Menus install from standard `MENU` resources, with optional `MCMD` four-byte
command records, or the original Finder's version1 `fmnu` representation. The
manifest compiler accepts either string items or item objects with `text`, `key`,
`mark`, `style`, `enabled`, `checked` and four-character `command` fields. Finder
resolves its own dynamic templates; generic menu services contain no Finder
action dispatch or localized labels.

Mouse tracking switches menus while held, highlights enabled items, clears the
highlight outside or on disabled/separator rows, and cancels on an invalid
release. Keyboard command equivalents enter the same native `MenuCommand` event
path. The event preserves signed 16-bit menu IDs and one-based item indices;
`Menus::selection` resolves their command metadata through the service.

The one-byte Apple title uses original System icon family -16386 through resident
Icon Utilities; title selection preserves its rainbow colors.

The aggregate userspace tests install captured menu records and run a private
SDK-only guest with real virtio input and the loaded MDEF. Original System7.6.1
MenuProbe captures supply strict, unmasked RGB crops for the File popup's normal,
highlighted and disabled-hover states. Capture transport performs no behavioral
assertions; Rust acceptance remains under `crates/mac-toolbox/tests/menus.rs`.

This is a bounded native implementation: only MDEF0 is installed, menu item count
is limited to 31 and installed menus to 8. Apple/Label icon rows, hierarchical and
scrolling menus, tear-off/pop-up placement, arbitrary custom MDEFs, complete
MacRoman mark/key handling, dynamic insertion/deletion and original menu flashing
are not established by the current oracle. Original classic MenuSelect/MenuKey
trap adaptation is a separate gate; native `MenuCommand` is not claimed as a
classic Event Manager event. Current runtime ownership is the foreground
application; multi-application menu bars require the scheduled application
contexts described in `THREADING.md`.
