# Native applications

Application target policy: [P8](../docs/RUST-STD.md#application-build-and-default-panic-contract)
makes std the default for all native userspace apps, including Finder. Applications
will use normal main without no_std/no_main, custom loader entries or per-app
panic macros. The platform default panic path must display the System-owned bomb
dialog. This migration is planned; the applications below still use the working
no_std entry/panic path until the target/sysroot/runtime replacement is verified.

[P11](../docs/TERMINAL.md) plans a Terminal APPL hosting new terminal-only CLAP
applications. Terminal preferences define their base environment; commands run
as separate processes in validated sessions. [P12](../docs/HOSTED-TOOLCHAIN.md)
then stages hosted Rust compiler/build tools. These are not installed apps yet.

`cargo xtask image` builds and packages these applications into the startup
volume's `Applications` folder. Each application has its own native entry point,
Cargo-generated `vers`, `RV64` code, `NABI` descriptor, `MENU`/`MCMD` commands,
window layout and icon bundle. Applications call OS services through `mac-toolbox`;
the resident implementation remains in System. ResEdit additionally uses the
allocation-free `mac-text` scalar table to decode raw MacRoman resource payloads.

| Application | Cargo package / executable | Initial memory reservation |
| --- | --- | --- |
| QuickDraw Test | `quickdraw-test` / `QuickDrawTest` | `0x83000000..0x84000000` |
| TextEdit Exerciser | `textedit-exerciser` / `TextEditExerciser` | `0x84000000..0x85000000` |
| Cursor Test | `cursor-test` / `CursorTest` | `0x85000000..0x86000000` |
| Panic Test | `panic-test` / `PanicTest` | `0x86000000..0x86800000` |
| ResEdit | `resedit` / `ResEdit` | `0x86800000..0x87000000` |

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

Panic Test deliberately calls Rust `panic!` through Panic Now (Ctrl-P) and
Long Panic (Ctrl-L), with equivalent buttons and a normal Quit (Ctrl-Q).
The short case exercises formatted values, Unicode, and automatic source
location capture. The long case includes over 20 KiB of numbered Unicode
diagnostics and `END_OF_LONG_PANIC` at its end, to exercise complete reporting
and scrolling. It uses the shared Toolbox panic handler, as do the other apps.

ResEdit is the read-only resource browser, with original ResEdit2.1.3 icons.
Its preferred reservation is `0x86800000..0x87000000`; Panic Test's reservation
now ends at `0x86800000`. Open apps or other resource files from its File menu,
including apps that are running. It previews icons, menus, strings and versions,
and exports a lossless resources.json archive to the host. See
[ResEdit](../docs/RESEDIT.md) for export/rebuild commands and phase boundaries.

ResEdit browses resource forks of arbitrary files, including running apps, and
exports a lossless resources.json archive using native Save/Save As. Its Open
and Save dialogs belong to the shared System Standard File Package. The icon
bundle is copied from original ResEdit 2.1.3. See [ResEdit](../docs/RESEDIT.md).
