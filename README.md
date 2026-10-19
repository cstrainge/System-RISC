# RISCy System

RISCy System's Rust workspace lives under `System7/`. It contains build tools
and independently linked native RISC-V **System** and **Finder**
prototypes, packaged as Macintosh files with real resource forks.

The [Toolbox plan](TOOLBOX-PLAN.md) defines the remaining managers, dependencies
and conformance gates. All canonical Rust compatibility tests belong in the
aggregate `crates/mac-toolbox/tests` and call public userspace APIs. The shared
suite runs on the host and through real RISC-V ECALLs via `cargo xtask check`.
Run its host side with `cargo test -p mac-toolbox --tests`.

The [boot ROM](bootrom/README.md) cold-boots from QEMU's read-only flash,
finds a blessed HFS System Folder, loads its native System resource executable,
and starts System using disk reads performed inside the guest. System loads
Finder from that same blessed folder, initializes graphics and input, and
provides window, drawing and event services to its event loop. Standard WDEF 0
and button CDEF0/scrollbar CDEF1 are independently linked executables in System's
resource fork.
The [Window Manager](system/src/toolbox/windows/README.md) supports activation, occlusion,
exposure updates, dragging, resizing, closing and zooming. Its standard definition
uses [System 7.6.1 WDEF 0 evidence](../analysis/wdef-standard.md) and original
Chicago 12 lettering. Its color drawing matches all 29 retained original-Mac
cases exactly, including all eight standard variations and pressed controls.

This is an initial native desktop, with eight windows, retained drawing commands
and a single foreground application. Finder shows
real mounted HFS volumes and folder contents. Double-click volume/folder icons
to open them; existing folder windows are selected rather than duplicated.
Selection, open-folder icons, scrollbars, invisible flags and saved icon positions
are handled by Finder. **A** opens About and **Q** opens the resizable
**QuickDraw Exerciser**. Menu labels remain static. The exerciser uses the public Toolbox bindings
to test shapes, transfer modes, regions, bitmaps, fonts, offscreen worlds, PICTs,
original icon families and the shared Resource Manager. Keys **1–9** select pages,
**Left/Right** browse icon families on page 8, and **Q** reopens the window.
Original 68k execution, complete Toolbox coverage and full Finder rendering
remain future work. The earlier `riscv-quickdraw` demo remains separate.

## Build and check

Run from this directory:

```sh
cargo xtask build
cargo xtask image
cargo xtask boot
cargo xtask boot-check
cargo xtask inspect ../build/system7/dist/Finder.bin
cargo xtask check
```

For the graphical desktop, from the repository root:

```sh
python3 System7/scripts/system7_vm.py start
python3 System7/scripts/system7_vm.py check --no-build
python3 System7/scripts/system7_vm.py stop
```

The launcher opens a separate native Windows GTK window at 2× when available,
otherwise Linux GTK. It copies the generated ROM and disk into
`working/system7-vm` before attaching them read-only. Its QMP port is 4447;
the classic Mac and old QuickDraw machines keep their own state and ports.
Click a window to activate it, drag its title, resize from the lower-right,
close from the upper-left, and zoom the volume window from the upper-right.
**N** opens/selects the startup-volume window; **Ctrl-W** closes the front window.
**Return/Ctrl-O** opens the selected folder, **Ctrl-Up** opens its parent,
and **Page Up/Page Down** scroll the front folder. Scrollbar arrows, tracks and
thumb dragging also scroll the view. **A** opens About; **Q** opens QuickDraw.
For now, either host Ctrl key produces the guest Mac Command modifier. Host
Meta/Windows remains a Command alias; guest shortcut semantics stay unchanged.

`build` compiles both guest programs and the flash ROM with Cargo's locked dependency versions,
generates resource identifiers and linker layouts from their manifests, and
packages their executables and resources. `image` also creates a separate
500 MiB HFS volume and qcow2 image. `check` performs formatting checks, host unit
tests, both cross-builds, HFS fork/metadata roundtrips, qcow2 checks and byte
comparison, isolated headless QEMU execution of both packaged ELF payloads,
and cold boots through the ROM from actual attached disks. It also tests the
graphical desktop with QMP input through the actual virtio keyboard/tablet and
compares framebuffer regions for exposure and close/reopen behavior. It also
verifies that applications and definitions contain no resident manager code or
font strikes, checks the native and classic trap adapters, checks the production RV64 frame against the validated WDEF renderer and runs
strict RGB comparisons against the original-Mac reference captures. Run
`python3 scripts/check_wdef_pixels.py` from the repository root for that
appearance check alone. The combined check also compares QuickDraw primitives,
icons and fonts against retained original-System captures and verifies every
imported resource in the packaged forks. TrueType and synthetic-style pixel
differences are quantified separately from the strict bitmap/metric checks;
passing does not imply full font scaler parity. `scripts/capture_wdef_reference.py` reproduces the
original captures using a private clone of the historical classic-Mac disk.
`test` is an alias for `check`.

The separate ABI check extracts executable resource 0 from each MacBinary package and
loads those ELF files using QEMU's generic loader. A small RV64 harness calls
System, gets its service table, launches Finder through its entry point, and
checks rejection of incompatible ABI headers and null application arguments.
It exits with `ABI_SMOKE_PASS` on success. This exercises actual independently
linked RISC-V code and their shared interface. **It does not boot from HFS.**
The ROM checks additionally exercise the real virtio/HFS/resource/ELF loading
path, renamed blessed folders, Apple partition maps, disk fallback, and startup
file rejection. `cargo xtask boot` opens a serial console and boots the generated
disk read-only. `cargo xtask boot PATH` builds just the ROM and uses an existing
raw or qcow2 disk without modifying it. `cargo xtask rom` builds just the ROM.

Prerequisites:

- Rust, Cargo and rustfmt. The supplied `rustfmt.toml` uses nightly formatting
  options; the ROM also uses nightly's allocation-error handler. The workspace
  was verified with the installed nightly toolchain.
- The `riscv64imac-unknown-none-elf` Rust target. If needed, install it with
  `rustup target add riscv64imac-unknown-none-elf`.
- `qemu-img` for `image`/`check`, and Linux `qemu-system-riscv64` for `check`.
- `llvm-objcopy` for converting the boot ROM ELF into the flash image.
- Python 3 with Pillow for desktop and original-reference pixel checks.
- The existing workspace-local hfsutils installation for `image`/`check`.
  If absent, run `python3 ../scripts/install_hfsutils.py`.
  `SYSTEM7_HFSUTILS` can specify another directory containing those executables.

The checked-in extracted assets suffice to build these Rust packages. Reproducing
the original captures and resource extraction needs the reference System 7.6.1
installation, font suitcases, ROM and Retro68 toolchain described in the analysis
documents. Cargo downloads the host tools' dependencies
on the first build. Guest code uses no `std`. The ROM and System supply their
own allocators; Finder and the definitions require no heap.

## Layout

```text
System7/
  Cargo.toml, Cargo.lock, rustfmt.toml
  system/                  Resident System executable and resources
    src/machine/           Hardware and RV64 trap entry
    src/toolbox/           Resident manager implementations
  finder/                  Finder executable and resources
  bootrom/                 Read-only flash firmware and virtio-block driver
  definitions/             Independently linked native WDEF and CDEF resources
  crates/
    mac-abi/               Versioned native service/entry-point contracts
    mac-traps/             Typed transport over the loader-supplied trap gate
    mac-toolbox/           Rust service bindings and Toolbox facade
    mac-formats/           no_std + alloc binary formats, reusable in guests
    mac-hfs/               no_std + alloc HFS reader with caller-provided sector I/O
  toolbox/
    quickdraw/             Thin Rust drawing and geometry bindings
    windows/               Thin Rust Window Manager bindings
    events/                Thin Rust event bindings
    controls/              Thin Rust control bindings
    resources/             Thin read-only Resource Manager bindings
    files/                 Thin read-only File Manager bindings
    memory/                Client namespace for future services
    menus/, dialogs/       Client namespaces for future services
  tools/
    mac-build/             Host resource compiler and guest build support
    xtask/                 Build, inspect, HFS adapter and check commands
  tests/
    abi-smoke/             Bare-metal native entry/service test harness
```

The public Toolbox crates provide safe Rust bindings over a single resident
trap gate. Manager implementations live under `system/src/toolbox`, with matching
modules for each area. `mac-abi` contains only shared layouts and value types;
`mac-traps` owns the transport boundary. System does not depend on its public
client bindings. See [ARCHITECTURE.md](ARCHITECTURE.md) for the dispatch path and
current classic-trap coverage.

WDEF and CDEF own their definition-specific geometry, hit testing and drawing in
each resource's `src/main.rs`. They call OS drawing and font primitives through
QuickDraw bindings. The resident Window Manager loads and invokes their entry
points; it contains no standard definition implementation. Host tests reuse those
same source files without linking them into production System.

`mac-formats` has no filesystem or subprocess dependencies. Its resource fork,
MacBinary, text, native descriptor and ELF code can be reused by a future
native disk utility once a guest allocator exists. `tools/xtask/src/disk.rs`
is an initial **host adapter** around hfsutils. A native disk application will
need HFS write support and writable block services. The File Manager now exposes
read-only mounted-volume/catalog/fork services through native traps. `mac-hfs`
provides the shared HFS engine used by both the actual boot ROM and resident System;
the host adapter remains separate.

The existing `../riscv-quickdraw` and copied `../sorth` projects remain separate.

## QuickDraw, fonts and resources

All rasterization and font interpretation run in System. Public bindings expose
stateful window ports, region algebra, bitmap transfers, direct-RGB offscreen
worlds, cursors, PICT playback and icon families. Applications borrow their input
buffers; System owns snapshots needed for later repaint. The original font
assets remain resources in System: 83 resources across nine families, including
17 TrueType faces. Bitmap strikes are selected through their FOND associations;
other sizes use the original sfnt hint programs through the resident no_std
Skrifa interpreter. Ordinary application images contain neither these fonts nor
the rendering engines.

The System fork includes 736 original visual resources; Finder includes 169.
The imported payloads, IDs, names and attributes are preserved, with explicit
ROM fallbacks for absent System resources. Imported font suitcase collisions are
remapped together with their FOND associations. Extraction manifests and source
hashes live in `../provenance/system7-{font,visual-resource}-bundle.json`.

`system.resources()` searches application resources before System, or callers
can select `.application()` / `.system()`. It provides type/ID and case-sensitive
name lookup, type/resource enumeration, size, name, attributes, origin, partial
reads, load and logical release. Enumeration is zero-based in Rust; duplicate
IDs in separate files remain separately enumerable. Resident fonts share this
same lookup path after transfer into the font store. Resources are read-only and
pinned: resource-file editing, purgeable handles and the original 68k Resource
Manager ABI are not yet implemented. See [Resource Manager](toolbox/resources/README.md).

The original comparisons and remaining API gaps are documented in
[QuickDraw coverage](../analysis/quickdraw-coverage.md),
[font comparisons](system/src/toolbox/quickdraw/fonts/README.md),
[PICT replay](../analysis/quickdraw-pictures.md) and
[icon comparisons](../analysis/icons-reference.md). Native API coverage and
original A-line adapter coverage are tracked separately.

## Artifacts

Generated files live in `../build/system7/`, outside the source tree:

| File | Purpose |
| --- | --- |
| `dist/System.elf`, `dist/Finder.elf` | Linked executables, with debug information |
| `dist/*.runtime.elf` | Executables stripped of symbols/debug for embedding |
| `dist/System.rsrc`, `dist/Finder.rsrc` | Standard big-endian resource forks |
| `dist/System.data`, `dist/Finder.data` | Data forks, currently empty |
| `dist/System.bin`, `dist/Finder.bin` | MacBinary II transports preserving forks and metadata |
| `dist/System.json`, `dist/Finder.json` | Resource inventories, entry addresses, segments and hashes |
| `dist/build.json` | Package inventory and compiler/tool versions |
| `dist/bootrom.elf`, `dist/bootrom.bin` | Debuggable firmware and 32 MiB flash-bank image |
| `dist/bootrom.json` | Firmware size, address and hash |
| `dist/system7.hfs` | Sparse, 500 MiB unpartitioned HFS volume |
| `dist/system7.qcow2` | Same volume in qcow2 format |
| `dist/disk.json` | File fork verification, volume blessing and disk checks |
| `abi-smoke/serial.log` | Actual System/Finder execution output |
| `boot-checks/*.serial.log` | Cold-boot and rejection-case output |

`../results/system7/toolchain-checks.json` records the complete check result.
`../results/system7/boot-checks.json` records the disk boot suite separately.
`../results/system7/desktop-checks.json` records cold boot and desktop interaction;
`desktop-input-checks.json` contains the individual input/pixel checks.
`wdef-appearance-checks.json` contains the 29 original-reference RGB comparisons;
`guest-wdef-checks.json` checks the loaded RV64 frame against that renderer.
`toolbox-boundaries.json` records dependency and linked-code ownership checks.
The color milestone and source/artifact hashes are retained in
[`provenance/riscy-system-color-wdef.json`](../provenance/riscy-system-color-wdef.json).

The HFS image contains `System Folder/System`, `System Folder/Finder`,
`System Folder/Preferences`, `Applications`, and `Documents`. The System Folder
is blessed. System has type/creator `zsys/MACS`; Finder has `FNDR/MACS`, matching
the file identities inspected on the reference installation CD. HFS copying
verifies both forks, the filename and type/creator; qcow2 conversion is checked
against every raw disk byte.

Image generation uses isolated hfsutils mount state and a temporary image,
then replaces generated images under `build/system7/dist`. It never updates
the installed classic Mac disk or attaches disks to the visible QEMU session.
These are generated base images; future writable guest sessions should use
separate overlays so rebuilding cannot discard guest documents.

MacBinary filenames use `.bin` only for host transport. On HFS the files are
named simply **System** and **Finder**, with their actual resource forks.

Package dates use `SOURCE_DATE_EPOCH` when supplied, otherwise Unix epoch zero.
Identical ELF inputs, resource definitions and epoch produce identical packages.
HFS volume/directory timestamps are not normalized, so complete disk images
are not yet reproducible byte for byte across builds.

## Mounted volumes and Finder

The ROM retains every discovered block device and supplies bounded sector-read
callbacks to System. The resident File Manager mounts bare HFS and Apple_HFS
partitions, exposes volume/catalog metadata and both forks, and owns independent
seek marks and generation-checked open handles. Applications call
`system.files()`; no filesystem parser is linked into Finder. See
[File Manager](system/src/toolbox/files/README.md) for the API and limits.

Finder enumerates those volumes to build its desktop, using driver-supplied icon
identities. Folder windows read actual catalog entries, hide invisible entries,
and choose original folder/application/document icons. Open locations are tracked
by volume and catalog ID, so opening/closing a window updates its parent and
volume icons. The Window Manager owns composition and focus; Finder owns desktop
items, icon layout and captions. The standard scrollbar is independently linked
CDEF1, alongside WDEF0 and button CDEF0.

The view was measured on original System7.6.1: 64×52 candidate grid, Geneva9,
32-pixel icons, baseline42 below the nominal icon top, and single-line labels
whose padded bounds may touch. Original comparisons include normal, selected,
open and selected-open folders; one/two-volume desktop states; Trash; label
spacing boundaries; and scrollbar drawing/hit behavior. The native two-volume
check also compares actual RV64 Finder pixels after opening and closing folders.
See [original evidence](../analysis/finder-large-icons.md) and
`../results/system7/finder-{icon,volume}-checks.json`.

This slice is read-only: resource/data-fork writes, rename/create/delete,
unmount/hotplug, desktop-file placement, application launching and full classic
File Manager traps remain future work. Initialized ordinary catalog icon cells
are read using the original cell-to-icon offset; new positions are not written
back. Folder window geometry, view, scroll and startup open-window restoration
are not implemented yet. See the [original save/restart review](../analysis/finder-state.md)
and [persistence audit](../analysis/finder-persistence-audit.md) for the metadata
path and remaining restoration/write work. The capacity display uses native HFS
allocation totals; the measured original OS reserves some capacity under a policy
not yet established for every device. This is a tested reconstruction of the
large-icon view, not all Finder behavior.

The [Memory Manager's first handle slice](system/src/toolbox/memory/README.md)
provides owned clear/empty allocations, size/state/lock/purge operations,
resize/reallocation, copying/appending, disposal and safe byte access. Its48
original handle observations and267 original oval-region records run through
the aggregate userspace suite. Pointer APIs and full heap/zone behavior remain
separate work; the three captured pointer-clear cases are explicitly deferred.

## Resource definitions

Each executable owns a `resources.json`. It defines its file identity, ABI,
reserved load range and resource definitions. Cargo supplies the application version. The same manifest
generates its linker script and Rust resource constants in Cargo's `OUT_DIR`.
System and Finder build scripts use the shared `mac-build` crate.

Definitions support:

- `pstring`: Mac Roman Pascal strings in `STR ` resources.
- `strings`: count plus Pascal strings in `STR#` resources.
- `menu`: basic classic `MENU` resources, including item terminators.
- `hex`: arbitrary binary resource data, such as `SIZE`.
- `file`: binary data read relative to the manifest, for icons and other assets.
- `native_code`: descriptor plus stripped native ELF for an executable resource.

Each definition has a four-byte `type`, signed 16-bit `id`, unique uppercase
`symbol`, optional Mac Roman `name`, and optional resource `attributes`.
Compressed resources are not supported. The compiler adds `vers` resource 1
for Cargo-derived version metadata and generates constants such as
`resources::FILE_MENU`, of type `mac_abi::ResourceKey`.
See [Finder's definitions](finder/resources.json) for examples.

The packager reserves:

| Resource | Meaning |
| --- | --- |
| `'RV64'` ID 0 | ELF64 little-endian RISC-V executable, stripped for runtime |
| `'NABI'` ID 0 | Our native format version, image role, service ABI and reserved memory range |
| `'vers'` ID 1 | Generated classic version resource from Cargo package version |

`RV64` and `NABI` are new project formats. Existing 68k `CODE` resources retain
their original meaning; putting RISC-V code in a resource fork does not make
it executable on the original Macintosh.

## Native loading contract

See [FORMAT.md](FORMAT.md) for the exact descriptor and loader requirements.
For this first milestone, System reserves `0x80200000..0x81000000` and Finder
reserves `0x81000000..0x82000000`. They are independently linked fixed-address
executables. Relocatable/multiple application instances require later loader
work. The service interface uses `repr(C)` structures and `extern "C"`
functions, not Rust's internal ABI. It is native RV64. The initial classic adapter explicitly marshals 32-bit
pointers, Pascal stack arguments and big-endian structures for a supported
geometry/event/text subset; full 68k execution remains future work.

The ROM loads System from the blessed folder and lends it a read-only startup
resource-fork callback. System loads Finder, WDEF and CDEF, then owns graphics,
input and window state while Finder calls the service table. See
[CODE-RESOURCES.md](CODE-RESOURCES.md) for executable-resource dispatch and the
automatic translation hook. A working 68k translator is not included.

## Format sources

- [Apple: Resource File Format](https://dev.os9.ca/techpubs/mac/MoreToolbox/MoreToolbox-99.html)
- [MacBinary II standard, revised July 24, 1987](https://files.stairways.com/other/macbinaryii-standard-info.txt)
- [RISC-V ELF psABI](https://github.com/riscv-non-isa/riscv-elf-psabi-doc)
- [Rust C representation](https://doc.rust-lang.org/reference/type-layout.html#the-c-representation)


## Native PNG images

Finder's QuickDraw Exerciser includes a PNG page: press **Q**, then **0**, or click
**0 PNG**. It reads a PNG resource through the public Resource Manager and uses
resident QuickDraw to draw at natural size, scaled, with alpha, and clipped over
a checkerboard. Finder links only `mac-toolbox`, without a PNG decoder.

The initial native PNG profile supports static, noninterlaced grayscale, RGB,
indexed, grayscale-alpha and RGBA images, legal1/2/4/8/16-bit depths and tRNS
transparency. The service validates chunk CRCs and the exact compressed data
length, reports malformed input, and uses fallible bounded decode buffers.
Adam7 and APNG return `NOT_SUPPORTED`. Colour profiles/gamma are not applied.
Input is capped at1MiB, dimensions at1024peraxis and total pixels at262144;
limits and detailed errors are available through `quickdraw::images`.

Native runtime direction: every application will receive a real OS thread;
legacy cooperative contexts and app globals are virtualized inside its app
context. Rust `std` support will sit on the native runtime services. See the
threading/runtime contract in [ARCHITECTURE.md](ARCHITECTURE.md) for current
implementation boundaries. All future system preferences use versioned JSON.


## Source repository

`System7/` is the OS-only Git repository. Rust crates, application/definition
resources, native VM/checker scripts, docs and bounded test fixtures belong here.
Build output, ROMs, VM disks and private runtime captures stay in the enclosing
RE workspace. See [scripts/README.md](scripts/README.md) for harness prerequisites.
Commit completed, verified milestones here; do not add unrelated Strange Forth
source or generated target directories.
