# Application icons

Finder draws each APPL's own icon in folder windows and Get Info, including
before launch. The resident application menu draws the running application's
icon in its row and follows foreground changes in its menu-bar title.

Both paths resolve the creator/type through BNDL and FREF local-to-resource ID
mappings. They do not assume icon ID128. Menu icons retain the loaded image's
resource scope, so applications can reuse resource IDs without sharing artwork.
Finder's MACS/FNDR identity resolves through the original System bundle to family3.

The public QuickDraw SDK adds `Icons::plot_file` and
`SurfacePort::plot_file_icon`. Finder continues to depend only on `mac-toolbox`.
System owns disk access, bundle parsing, color/mask/size selection and retained
window drawing. File icon lookup leaves the application's resource search chain
unchanged. Missing or malformed file icons use Finder's existing fallback.

The file-backed reader loads a map of at most256KiB, at most64KiB of BNDL/FREF
metadata and at most64KiB of the selected icon family. It never reads RV64 code
or loads an executable just to draw an icon. Existing 518-byte vers requests use
the same bounded reader. The icon renderer prefers small resources at16px and
large resources at32px, then color depth; it preserves masks and selection/open
transforms.

## System folders and System types

Folders that belong to the installed layout draw their own glyph rather than the
generic folder. The mapping lives in one table,
`quickdraw::icons::standard::for_blessed_folder`, keyed by the folder's name and
applied only to folders directly inside the volume's blessed folder. The System
creates those folders, so their name is their identity; an ordinary folder with
the same name elsewhere keeps the generic glyph. `Drivers`, `Fonts`, `Extensions`,
`Control Panels`, `Preferences`, `Startup Items`, `Apple Menu Items` and `Pipes`
are in the table.

A type can carry its own document glyph without the Finder holding a table of
types. The `BNDL`/`FREF` pair in the System's resources does what it does for the
font suitcase: the bundle's signature is the creator, its `FREF` names the file
type and a local icon id, and its `ICN#` mapping carries the real family id, so
Icon Manager lookup resolves a creator/type pair to a family. `PIPE`/`PIPE`
resolves to `-30003` this way.

Native families live in the reserved block `-30031..-30000`, which
`file_icons.rs` guarantees does not collide with any original Macintosh icon id
and admits only the symbols the System actually defines: the Drivers folder and
driver document (`-30000`, `-30001`), the Pipes folder (`-30002`) and a named
pipe (`-30003`).

The Pipes families are derived from the original standard folder (`-3999`) and
document (`-4000`) artwork with a pipe motif added, so each is its own glyph
rather than a duplicate. `scripts/make_pipes_icons.py` regenerates them from the
extracted originals and `system/assets/icons/pipes/README.md` records the
provenance.

## Running files

Finder's APPL icon uses the open/stipple transform while
`Processes::is_running(volume, cnid)` is true. The query includes background and
hidden instances and compares both fields, never the name, creator, icon ID or
path. FNDR files stay normal. Launch and exit invalidate Finder-owned windows;
ordinary Window Manager damage coalesces the updates. No idle polling or
activation-triggered process scans are added.

Same-file launch brings its instance forward. Different file copies receive
independent relocated images and threads; see [native applications](../apps/README.md).
The SDK process gate runs two copies concurrently, checks separate mutable
statics, renames and moves one through Trash/Put Away, and verifies independent
running state through each exit. The graphical fixture verifies that an
unlaunched copy of the same QuickDraw application stays normal while its source
is running, then verifies the source returns to normal after exit.

## Validation

- `mac-toolbox/tests/file_icons.rs` exercised actual HFS reads through the SDK:
  non-128 IDs, overlapping IDs in different apps, large/small families, color
  precedence, transparency, malformed/missing resources, unchanged search chain,
  read-only media and bounded reads alongside a1MiB executable resource, plus the
  native-reservation allowlist that admits only the System's own families in
  `-30031..-30000`. It was removed with the host test suite
  ([testing policy](TESTING-POLICY.md)), so icon resolution against real HFS is
  **unverified** on the target until a `cargo xtask boot-check` case covers it.
- The pipes probe, which runs in the guest as a boot-check case, resolves the
  `Pipes` folder's icon through the shared blessed-folder table and a named
  pipe's through the `PIPE`/`PIPE` type bundle, plots both into offscreen worlds
  and compares the pixels with the generic folder and document. It reports the
  resolved ids and how many pixels each glyph drew, so a family that resolved but
  drew nothing fails the case.
- `mac-toolbox/tests/application_ui.rs` checked6,656 icon pixels across
  unlaunched Finder entries, application-menu rows and foreground titles, and a
  private copy that moves QuickDraw's icon to9433 without changing its executable.
  That suite was removed with the host test suite ([testing policy](TESTING-POLICY.md))
  and its pixel comparison is **unverified** on the target.
- The real Get Info cold boot and the SDK boundary checks that ran with the former
  suite are unverified too. The source System/Finder resource payloads remain
  unchanged. Disposable captures delete their disks while retaining frames/logs.

Reports live outside the repository in `../results/system7/application-ui-checks.json`
and `application-ui-windows-checks.json`. The existing original menu comparison
continues to exclude its256-pixel Finder icon slot: its captured artwork differs
from the current FNDR resource projection. The new icon assertions compare with
known packaged resources; they do not claim that excluded original slot matches.
