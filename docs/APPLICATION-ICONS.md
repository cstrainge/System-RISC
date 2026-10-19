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

- `mac-toolbox/tests/file_icons.rs` exercises actual HFS reads through the SDK:
  non-128 IDs, overlapping IDs in different apps, large/small families, color
  precedence, transparency, malformed/missing resources, unchanged search chain,
  read-only media and bounded reads alongside a1MiB executable resource.
- `mac-toolbox/tests/application_ui.rs` checks6,656 icon pixels across unlaunched
  Finder entries, application-menu rows and foreground titles. A private copy
  moves QuickDraw's icon to9433 without changing its executable. Both Linux and
  Windows native QEMU pass, including existing launch/switch/hide/restore tests.
- Host workspace tests, host/RV64 Clippy, SDK boundary checks and the real Get Info
  cold-boot regression pass. The source System/Finder resource payloads remain
  unchanged. Disposable captures delete their disks while retaining frames/logs.

Reports live outside the repository in `../results/system7/application-ui-checks.json`
and `application-ui-windows-checks.json`. The existing original menu comparison
continues to exclude its256-pixel Finder icon slot: its captured artwork differs
from the current FNDR resource projection. The new icon assertions compare with
known packaged resources; they do not claim that excluded original slot matches.
