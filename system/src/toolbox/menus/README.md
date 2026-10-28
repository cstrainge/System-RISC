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

A committed mouse selection flashes its item three times before the command or
System application-menu action is delivered. Deferred app commands retain the
selection-time timestamp, pointer position and modifiers. Command-key shortcuts flash the
menu title three times. Each off/on phase holds for at least three 60 Hz ticks;
late presentation delays a phase rather than skipping a flash. The resident
input/timer path owns this state, so MDEF never waits or spins. The selected row
or title is repainted through MDEF with bounded presentation damage; flash
phases do not recompose unrelated windows. Escape cancels pending feedback.
Foreground changes, menu disposal/mutation and application-menu membership
refresh cancel stale selections before delivery.

MDEF draws a black highlight with white text and marks. Application-menu title
and item icons keep their normal colours and resource flags, including hidden
application dimming; selection does not invert icon pixels. The private native
`DRAW_ITEM` definition message repaints the one-based row supplied in `param`,
using the same row renderer as a full popup. Rebuild System and MDEF together.

## Planned System-owned Apple menu

Status: backlog; drawing an installed Apple-title MENU resource does not yet
guarantee a persistent System-owned Apple menu. Track delivery in
[roadmap P5](../../../../docs/CORE-TOOLBOX-ROADMAP.md#p5--system-owned-apple-menu).

- System always supplies exactly one leftmost Apple menu, just as it owns the
  right-hand application menu. It remains present when an app has no menus,
  clears/replaces its menus, switches, exits or crashes. System owns its title,
  tracking and global entries; applications contribute their own section.
- Provide a thin Rust Toolbox API for resource-based app contributions such as
  About, with explicit command identity, enabled/check state, insertion/removal
  and lifetime. Restore the foreground app's contribution on activation and
  remove it on exit; background changes must not change the visible app section.
  Localized labels, icons, separators and command metadata belong in resources.
- Compose app and System sections without duplicate Apple titles or separators.
  Keep System commands distinct from app commands and route selections once to
  the correct owner. Capture command identity/owner during tracking; cancel or
  validate pending selections across removal, exit and foreground changes.
  Do not depend on shifting visible item indices as native command identity.
- Preserve legacy MENU/MBAR installation and item-editing semantics through the
  legacy menu-list projection. Recognize Apple-title menus by their actual
  contract, not a hardcoded menu ID of 128; adopt their app section into the
  composed menu instead of adding a second title. Preserve the app's expected
  menu ID, one-based selection indices and MenuSelect/MenuKey results while
  keeping System entries out of app command dispatch. Specify logical-to-visible
  index mapping and legacy append/delete/query behavior before implementation.
- Use the shared generation-tagged facility for new resident contribution/object
  identities, with separate owner checks and cleanup. No app callbacks run under
  manager locks or block tracking of System-owned entries. Keep app commands on
  their own application thread; unavailable commands must not pretend to work.
- Populate global entries through actual System services and resource templates.
  Apple Menu Items folder contents and desk accessories require their respective
  file/alias/launch/legacy services; do not claim those merely from listing names.
  Cache composed layout/pixels and invalidate only changed menu bounds on actual
  content/state changes, never on every input or event poll.

Original reference: [Creating a Menu](https://dev.os9.ca/techpubs/mac/Toolbox/Toolbox-98.html)
defines an app's About item followed by a separator; ID 128 is a convention.
[Adding Items to a Menu](https://dev.os9.ca/techpubs/mac/Toolbox/Toolbox-102.html)
describes dynamic editing and Apple Menu Items. Capture the pinned System 7.6.1
section ordering, mutation, tracking and selection behavior, then encode public
SDK conformance tests for empty/cleared/replaced bars, contributions, ownership,
switch/exit/crash, stale IDs and bounded repainting. Add translated 68k MENU/MBAR
mutation and selection probes when the adapter exists. Automatic native menu
insertion is a System RISC ownership policy, not a claim that original apps did
not construct Apple menus themselves.

## Existing evidence and limits

The aggregate userspace tests install captured menu records and run a private
SDK-only guest with real virtio input and the loaded MDEF. Original System7.6.1
MenuProbe captures supply strict, unmasked RGB crops for the File popup's normal,
highlighted and disabled-hover states. Capture transport performs no behavioral
assertions; Rust acceptance remains under `crates/mac-toolbox/tests/menus.rs`.

This is a bounded native implementation: only MDEF0 is installed, menu item count
is limited to 31 and installed menus to 8. Apple/Label icon rows, hierarchical and
scrolling menus, tear-off/pop-up placement, arbitrary custom MDEFs, complete
MacRoman mark/key handling, dynamic insertion/deletion and original menu-flash timing parity
are not established by the current oracle. Original classic MenuSelect/MenuKey
trap adaptation is a separate gate; native `MenuCommand` is not claimed as a
classic Event Manager event. Current runtime ownership is the foreground
application; multi-application menu bars require the scheduled application
contexts described in `THREADING.md`.
