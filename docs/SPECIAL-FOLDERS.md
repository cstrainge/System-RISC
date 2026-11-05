# Special folders and their icons

How a folder becomes a *special* folder — Fonts, Preferences, Extensions, Control
Panels, Startup Items, the Apple Menu folder — and how it gets its icon. This is a
finding first and a design second, because the two halves of the mechanism that
should exist are not connected to each other.

## What exists today

**Icons are a classic family of standard IDs.** `toolbox/quickdraw/src/icons.rs`
declares the family as constants: the System Folder, Apple Menu Items, Startup
Items, Control Panels, Preferences, Extensions and Fonts each have an ID, plus a
generic folder and System RISC's own Drivers folder. The IDs are classic System 7
values, so the artwork and the constants agree.

**Which folder gets which ID is decided inside the Finder, by name.**
`apps/finder/src/browser.rs:287` (`folder_icon`) returns the System Folder icon
when the entry is the volume's blessed folder, and otherwise matches the entry's
**name as a string** when its parent is the blessed folder (`:298`–`:305`):

* `Drivers`, `Fonts`, `Extensions`, `Control Panels`, `Preferences`,
  `Startup Items`, `Apple Menu Items`,
* anything else is the generic folder.

**Files are decided differently.** The same file's caller takes files through
`for_file(creator, file_type)` (`:329`) — a type/creator binding. So in this tree
a *file* has an identity and folders do not.

**A folder-type vocabulary exists, and is nearly empty.** The filesystem service
has the classic shape already: `Files::find_folder(volume, folder_type, create)`,
backed by the volume's own `find_folder`, where the type is a four-character code
that maps to a name and a parent. It knows exactly three types:

| Type | Folder | Parent |
| --- | --- | --- |
| `desk` | Desktop Folder | volume root |
| `trsh` | Trash | volume root |
| `font` | Fonts | blessed System Folder |

Every other special folder has no type. Nothing can locate `Extensions`,
`Control Panels`, `Preferences`, `Startup Items` or `Apple Menu Items` by
identity; they exist only as names the image builder happens to create and that
the Finder happens to recognise.

## The finding

There is **no special-folder registry**. There are two name tables that do not
know about each other:

* the file manager's type table, three entries, used for locating;
* the Finder's icon table, seven entries, used for drawing.

Consequences, all of them live today:

1. **Renaming a special folder loses its icon and its identity.** Both tables key
   on the name. `Fonts` renamed is a plain folder; nothing reports that the
   system's fonts folder is now unreachable by type.
2. **A folder with the right name is claimed as special wherever it sits under
   the blessed folder.** The name is the whole proof of identity.
3. **The two lists disagree.** Seven folders are drawn as special; three can be
   found as special. A consumer that needs "the Preferences folder" has no way to
   ask for it, so it builds a path from a string — a second copy of the name
   table in a third place.
4. **Only a direct child of the blessed folder can be special.** The Finder's
   match is scoped to that parent, so a special folder at any other depth cannot
   be expressed at all.
5. **Another application cannot agree with the Finder.** A save dialog or a
   Installer-style window that draws a folder has to reproduce the Finder's name
   table, or draw the generic icon.

## The design

The mechanism should be the one the filesystem service already started: a folder
type, carried by the folder itself, with the icon bound to the type.

**Identity lives on the folder, not in a list.** A special folder's catalogue
entry carries its type — the folder's own file type/creator, which the volume
already stores per entry and which survives a rename, a move and a copy. The
type is the identity: two consumers that ask for `pref` reach the same folder
because they read the same entry, not because they agree about a string.

**The type table covers every special folder.** `desk`, `trsh` and `font` are
already there; `extn`, `ctrl`, `pref`, `strt`, `amnu` and `drvr` join them, each
with its parent rule (the blessed folder for the ones that live inside it, the
volume root otherwise). Creation and lookup use the same table, so
`find_folder(type, create)` both finds an existing folder of that type and creates
one with it when asked — which is what makes the type authoritative rather than
decorative.

**The icon binds to the type.** A single table maps folder type to standard icon
ID, held beside the icon constants rather than in the Finder. The Finder then asks
for a folder's type and gets an icon from that table; any other application that
draws a folder does the same and agrees. Renaming is harmless, a same-named folder
elsewhere is not claimed, and a folder at any depth can be special.

**New icons are ours.** A special folder that this system defines, rather than one
Apple defined, gets its own ID in the `standard` family and its own artwork — the
same rule already applied to the Drivers folder, whose ID is outside the classic
range. It is a distinct asset from any file icon in the same family, and it is
bound by type rather than extracted from an original resource.

## What this means for a Pipes folder

A `Pipes` folder is one entry in that table — a folder type, a parent, and an
icon ID — and nothing in the Finder. It should not be an eighth string in the
existing match, because that would add a row to the table that has to be
duplicated before anything outside the Finder can recognise the folder.

It is not added now. There is no producer of such a folder in this tree: the pipes
facility is specified and not implemented, so a type and an icon for it would be a
registration with no consumer — dead configuration, which this project removes
rather than adds. The table is the right place for it on the day the folder is
created by the facility that owns it.

## Renaming, decided

**A renamed special folder keeps its identity.** The type is on the folder, so a
rename moves the name and leaves the type alone.

The consequence is worth stating because it is a gain rather than a side effect:
a **new** folder that happens to be called `Fonts` is not special, because
identity is the type and not the name. Today the opposite is true in both
directions — a rename destroys the identity, and a look-alike acquires it. Both
disappear together.

Clearing a type is therefore a deliberate operation, not something that happens
as a side effect of moving a folder or editing its name. A system that wanted the
opposite behaviour would have to make the rename path clear the type on purpose,
and the burden of proof is on that change, because identity that survives is what
lets independent consumers agree.
