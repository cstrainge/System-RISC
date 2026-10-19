# Native applications

`cargo xtask image` builds and packages these applications into the startup
volume's `Applications` folder. Each application has its own native entry point,
Cargo-generated `vers`, `RV64` code, `NABI` descriptor, `MENU`/`MCMD` commands,
window layout and icon bundle. Their runtime dependency is only `mac-toolbox`;
the implementation of Toolbox services remains in System.

| Application | Cargo package / executable | Initial memory reservation |
| --- | --- | --- |
| QuickDraw Test | `quickdraw-test` / `QuickDrawTest` | `0x83000000..0x84000000` |
| TextEdit Exerciser | `textedit-exerciser` / `TextEditExerciser` | `0x84000000..0x85000000` |
| Cursor Test | `cursor-test` / `CursorTest` | `0x85000000..0x86000000` |

Open `Applications` in Finder and double-click an application, or select it
and use File > Open. The menu at the right edge of the menu bar switches running
apps and provides Hide, Hide Others and Show All. Every app, including Finder,
runs on its own timer-scheduled OS thread.

The resource manifest's `name` is the Macintosh display and HFS filename.
Optional `binary_name` selects a different Cargo executable name without spaces.
The manifests specify preferred link ranges. New packages also carry checked
`RLOC` metadata, allowing a copy to load at another free address.

QuickDraw Test includes ten pages for shapes, transfer modes, regions, bitmaps,
fonts, offscreen worlds, PICTs, icons, resources and PNGs. Use its Tests menu,
number keys or tabs to change pages. TextEdit Exerciser retains the native
editor's selection, caret, clipboard, resize and keyboard navigation behavior.
These applications return from their entry point on File > Quit, Ctrl-Q or closing
their only window; System owns thread termination and remaining object cleanup.

The QuickDraw test content and text sample are intentionally demonstration
data. User-facing window names, menu commands, tab labels and editor text come
from each application's resources. These are test applications, not a document
editor or a claim of original 68k application compatibility.

Cursor Test uses a fixed-size window and its own arrow icon. The large icon is
an exact 2× expansion of the bundled ROM arrow; the small icon uses its native
16×16 bitmap and mask. It provides labeled hover areas for Arrow, I-beam, Crosshair, Plus,
Watch, animated watch, and the original ring and RAM-disk cursor resources.
Its editable text box exercises automatic I-beam switching and typing obscures
the pointer until it moves. The Cursor menu has Hide, Show, Obscure, Reset,
Next Frame and Toggle Read Only commands (Ctrl-H/S/O/R/N/L).
It holds an owned Toolbox `Window`, so early returns also clean up its window.

New launchers use the public Toolbox facade:

```rust
let file = system.files().lookup(volume, 2, "Applications/QuickDraw Test")?;
let running = system.processes().launch(volume, file.id)?;
system.processes().activate(running.id)?;
```

Launching the same mounted-volume/catalog ID activates its existing process.
A separate copy has a different catalog ID and runs in its own OS thread with
separate code, data, BSS, and Toolbox state. Rename and same-volume moves preserve
that identity. Finder marks only that running file with the open-icon transform;
Finder itself is exempt. Hidden applications still count as running.

The loader retains eight immutable cached images, including Finder, and assigns
nonoverlapping physical spans. Supported RV64 relocations are checked before any
write; old fixed packages without `RLOC` cannot be moved to another link address.
This remains trusted shared memory without MMU protection. Original 68k execution,
document-open parameters and general dynamic linking remain later work.
