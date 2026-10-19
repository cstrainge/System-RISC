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

Open `Applications` in Finder and double-click either application, or select it
and use File > Open. The menu at the right edge of the menu bar switches running
apps and provides Hide, Hide Others and Show All. Every app, including Finder,
runs on its own timer-scheduled OS thread.

The resource manifest's `name` is the Macintosh display and HFS filename.
Optional `binary_name` selects a different Cargo executable name without spaces.
The reservations are the first native loader profile, not relocation support.

QuickDraw Test includes ten pages for shapes, transfer modes, regions, bitmaps,
fonts, offscreen worlds, PICTs, icons, resources and PNGs. Use its Tests menu,
number keys or tabs to change pages. TextEdit Exerciser retains the native
editor's selection, caret, clipboard, resize and keyboard navigation behavior.
Both applications return from their entry point on File > Quit, Ctrl-Q or closing
their only window; System owns thread termination and remaining object cleanup.

The QuickDraw test content and text sample are intentionally demonstration
data. User-facing window names, menu commands, tab labels and editor text come
from each application's resources. These are test applications, not a document
editor or a claim of original 68k application compatibility.

New launchers use the public Toolbox facade:

```rust
let file = system.files().lookup(volume, 2, "Applications/QuickDraw Test")?;
let running = system.processes().launch(volume, file.id)?;
system.processes().activate(running.id)?;
```

Launching an already running file activates its existing process. This first
native profile uses fixed nonoverlapping ELF regions and an eight-image cache;
original 68k application execution and document-open parameters are later work.
