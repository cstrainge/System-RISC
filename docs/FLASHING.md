# Flashing a card

## Before anything: the firmware the card needs

A bootable card needs the board's own firmware, and the build will not produce an
image without it. Copy the four files into a `firmware/` directory at the
repository root, keeping the names as published:

| File | What it is |
| --- | --- |
| `bootcode.bin` | first-stage loader the board's GPU runs from the card |
| `start.elf` | second stage: initialises the board and loads the kernel and the tree |
| `fixup.dat` | relocates `start.elf`'s sections so it can run |
| a board device tree, e.g. `bcm2710-rpi-3-b.dtb` | the firmware's own description of this board variant during bring-up |

All four come from the firmware distribution's `boot/` directory. **The board
device tree is not hardcoded**: `platforms/raspi3b.json` names it in
`hardware.board_device_tree`, because the correct file differs between a 3B and a
3B+ and a build that inferred it would ship a tree for the wrong board.

Three facts about these files:

* **They are not in this repository and must not be added to it.** They are
  Broadcom binaries, redistributable only unmodified, only for their own hardware,
  and only with their licence travelling alongside. `firmware/` is gitignored;
  the image carries them and the checkout does not.
* **The board device tree is copied byte-for-byte**, under the name the firmware
  publishes, because it *is* the firmware's contract during bring-up. Regenerating
  or rewriting it would mean we were testing our description rather than the
  machine's, and its hash would no longer identify what a board run used. The boot
  partition carries the files under their published names, which needs long-name
  entries: the writer emits them, and gives each such file a generated `NAME~1.EXT`
  short alias alongside.
* **The image records every firmware file's sha256** in `dist/disk.json`, so the
  same freshness gate that catches a rebuilt kernel catches a swapped blob or a
  regenerated tree.

If a file is missing, the image stage refuses by name and lists exactly what to
fetch — it does not emit an image whose partition table and kernel verify and
which cannot boot.

**An image built here must not be published**: it contains the firmware, and the
licence permits redistribution only for Raspberry Pi hardware and only with the
licence alongside. Build it for your own card.

Our own kernel does not use the firmware's tree: `hardware.device_tree` says
`generated`, so the kernel walks the tree this build produces. The firmware loads
its tree before any of our code runs, so it is required regardless — the two are
separate and neither replaces the other.

`cargo xtask flash TARGET --yes` writes this platform's declared storage image to
a card. **It erases the card.** Everything currently on the target is destroyed.

## The rules the tool enforces

- **You name the target.** The tool never chooses a device, never looks for "the
  only removable disk", and never guesses. If you do not name one, it refuses.
- **Nothing is written without `--yes`.** Without it the command still runs: it
  resolves the image, reads the target's size and prints both plus what is about
  to be erased, then exits non-zero having written nothing.
- **The image comes from the platform record.** It is whatever
  `hardware.boot_image` names, resolved the same way the emulator resolves its
  media. There is no path argument, so the tool cannot be pointed at an arbitrary
  file to write to a card.
- **Pre-flight before the first byte.** It refuses if the image is missing, if
  the image is empty, and if the image is larger than the target.
- **Verify by default.** After writing it re-reads the region and compares it
  byte for byte, so a truncated or failed write is reported rather than assumed
  good.

## Procedure

1. Build the image: `cargo xtask build`. The image lands in
   `dist/<platform>/<hardware.boot_image output>`.
2. Identify the card. On Windows this is `\\.\PhysicalDriveN`; check the number
   with `Get-Disk` before trusting it. **The number is not stable across
   replugging.**
3. Dry run it:
   ```
   cargo xtask flash '\\.\PhysicalDrive2'
   ```
   Read the reported capacity and the existing contents. If the capacity is not
   the card you are holding, stop.
4. Write it, from an **elevated** shell:
   ```
   cargo xtask flash '\\.\PhysicalDrive2' --yes
   ```

A file path is accepted in place of a device, which is how the write and verify
path is exercised without hardware.

## Windows specifics

A physical drive is a raw device, not a file. It is opened with no buffering and
write-through, so writes must be whole sectors and the image is zero-padded to
the next sector boundary. Opening it needs elevation; the tool reports permission
denied as "needs an elevated shell" rather than surfacing the bare OS error,
because the raw OS error for this case does not say that.

## What this tool does not do

- It does not format, partition, or copy individual files. It writes the
  platform's declared image whole. If you only want to replace one file on an
  existing card, that is a different operation and this tool will erase
  everything instead.
- It does not read a card, so it cannot back up what it is about to destroy.
- Verification re-reads through the same handle. It proves the write reached the
  medium as far as the operating system can report; it is not a surface scan.

## Testing

`tools/xtask/src/flash.rs` carries host tests for the write-and-verify round trip
against a temporary file, a truncated write, a differing medium, an oversized
image, an empty image, and the plan's report of existing contents. A flash tool
that has never written anything is not a working flash tool, so the round trip is
the test that matters.
