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

## System-owned Apple popup

System supplies the leftmost Apple title and a real popup in every application,
including applications with no menu resources or a cleared menu list. System's
`MENU`/`MCMD` -16488 supplies **About System RISC…**; System handles the command
and opens its own resource-backed About dialog through the resident Alert,
Window and Control managers. Finder uses the same service and no longer owns a
second System About window. Return, Escape or the managed OK button dismiss it.
An existing modal dialog or crash report refuses a programmatic About request
with `ServiceBusy`; background callers receive `AccessDenied`.

The native SDK exposes `menus::APPLE_MENU` for read-only layout/item queries and
`Menus::about_system()` to open About directly. Attempts to mutate the System
menu are refused. System actions never generate an application MenuCommand.
The About protocol is an ordinary registered modal record in the foreground
application's domain: System consumes its input and updates, and normal domain
cleanup releases it on application exit or failure. Applications cannot consume
it through the public Alert API.

An application installs an Apple section using an ordinary owned MENU resource
with the one-byte Apple title (`0x14`). Resource ID and installation order do not
identify that section. Its rows precede a single separator and the System rows;
leading/trailing and adjacent separators are omitted from the visible projection.
A second app Apple section is refused. Logical app item indices, menu IDs and
commands remain unchanged: a captured visible row maps to its original command
and generation-checked menu owner. App queries retain the logical item count;
layout reflects the composed popup. Disposing/clearing an app section removes
its rows while retaining System entries.

The current MDEF limit permits 29 logical app Apple items plus the separator and
System item. Oversized sections are refused before publication. Composition uses
preallocated System scratch state and runs when the source owner, handle or
revision changes; unrelated menu edits and input polls do not remeasure it.
Foreground changes restore the correct section. Mutation/disposal, foreground
changes and Escape cancel stale tracking or pending flash feedback. Selection
uses the same three-flash acknowledgement and bounded row/title painting as
ordinary menus. No application callback runs inside menu tracking.

Menu definitions return paint failures to the compositor. A refused title or
popup draw defers the frame for retry; a partially drawn bar never becomes the
cached snapshot. The previous displayed frame remains intact. Native menu IDs
reserved for the System Apple and application menus cannot be installed through
an app resource, including resources whose internal MENU ID differs from their
resource number.

Apple Menu Items folder launching, desk accessories, hierarchical/scrolling menus
and original 68k MENU/MBAR mutation/selection projection remain separate work.
This native System RISC ownership policy does not imply that original Macintosh
applications never constructed their own Apple menus. See
[roadmap P5](../../../../docs/CORE-TOOLBOX-ROADMAP.md#p5--system-owned-apple-menu).

## Existing evidence and limits

Runtime evidence must exercise the optimized native System, loaded MDEF and
real application services. Host-linked Rust suites do not establish those paths.
The System Apple slice needs graphical checks for default and composed popups,
System About, logical item routing, state changes, disposal/clear, foreground
switches, error/cancellation/retry and owner cleanup. Original Macintosh fidelity
and legacy trap compatibility require their own reference captures and probes.

This is a bounded native implementation: only MDEF0 is installed, menu item count
is limited to 31 and installed menus to 8. Apple/Label icon rows, hierarchical and
scrolling menus, tear-off/pop-up placement, arbitrary custom MDEFs, complete
MacRoman mark/key handling, dynamic insertion/deletion and original menu-flash timing parity
are not established by the current oracle. Original classic MenuSelect/MenuKey
trap adaptation is a separate gate; native `MenuCommand` is not claimed as a
classic Event Manager event. Application menu state follows the foreground saved
context. The persistent System title and right-hand application menu are
desktop-owned.
