# AArch64 hardware self-test

A single-purpose `kernel8.img` for a BCM2837-class board. The board's firmware
loads it raw at `0x80000`; it runs a fixed list of hardware checks, draws the
results and a QR encoding them, then alternates with a cache write-back
demonstration. It never reboots and never writes to the SD card.

## Build

```
cargo build --manifest-path selftest-aarch64/Cargo.toml --release --target aarch64-unknown-none
```

The firmware does not parse an ELF, so convert the linked image to a flat binary:

```
rust-objcopy -O binary selftest-aarch64/target/aarch64-unknown-none/release/selftest-aarch64 kernel8.img
```

`rust-objcopy` lives beside the pinned toolchain, for example
`%USERPROFILE%\.rustup\toolchains\nightly-2026-10-05-x86_64-pc-windows-msvc\lib\rustlib\x86_64-pc-windows-msvc\bin\rust-objcopy.exe`.
`build.ps1` runs both steps and checks the result.

Check the flat binary rather than assuming: its first four bytes must not be
`7f 45 4c 46`, and the entry instruction is the first byte of the file.

## Put it on a card

1. Rename the card's existing `kernel8.img` to `kernel8-linux.img` (the backup).
2. Copy this `kernel8.img` to the card root.
3. `config.txt` must have `dtoverlay=disable-bt` so the console is on PL011
   (`ttyAMA0`) rather than Bluetooth. Remove the KMS overlay
   (`dtoverlay=vc4-kms-v3d`) and `disable_fw_kms_setup=1` for this binary: a
   bare-metal image never sets up KMS, and inheriting the overlay can allocate a
   framebuffer that never scans out.
4. To revert, delete this `kernel8.img` and rename `kernel8-linux.img` back.

The firmware handover pointer in `x0` is ignored; nothing in the device tree or
the kernel command line is parsed. Every address and the display geometry come
from the platform record the binary was built against.

The PL011 console runs at 115200 8N1 and carries the same result lines as the
screen, plus the exact QR text between `SELFTEST_QR_BEGIN` and `SELFTEST_QR_END`.

## What it does

Screen 1/2 (results, 20 s) carries the summary, the QR and the per-check lines.
Screen 2/2 (cache demo, 10 s) shows the same QR written once with a clean and
once without. Photograph screen 1.

The checks are: mailbox board revision, firmware revision, core clock, ARM
memory size, framebuffer acquire, framebuffer write/read-back, SDHCI sector 0
and its MBR, the system timer, the MMU/caches, the cleaned read-back, and the
uncleaned read-back.

## Reading the result

`SELF TEST n OK m FAIL` is the summary; the QR carries the same per-check lines,
so photograph screen 1/2 rather than 2/2.

Two checks are easy to misread, and both are about the data cache:

- **`cache-clean`** writes `0xa5`, cleans the lines with `dc cvac`, and reads
  them back through a mapping of the same page that is marked non-cacheable.
  It must read back `0xa5` at every byte. A failure means the clean never
  reached memory, and the display would tear.
- **`cache-dirty`** writes `0x5a` and does *not* clean, then reads through the
  same non-cacheable mapping. It must read back **`0xa5`** -- the earlier
  pattern -- because the `0x5a` is still in the write-back cache. Reading back
  `0x5a` means the store reached memory without a clean, which is the condition
  that makes `dc cvac` pointless.

The non-cacheable alias exists because no cached read can show the difference:
`dc ivac` would have been the obvious way to look at memory, but on Cortex-A53
it cleans a dirty line before invalidating it, so it writes the very data the
test is trying to keep out of memory.

Expected on silicon: every check passes. Expected on QEMU: `cache-dirty` fails
with `no-drift`, because TCG has no data cache, so memory always holds the last
store and the alias sees it.

`sdhci` is the one check whose *failure text* matters. A refusal names the
command, both interrupt status registers and the capabilities register, for
example `cmd 41 int 0x8001 err 0x0 cap 0xc00`. `cmd 8` with `err 0x1` is a
command timeout, which is what an empty slot produces.

Board revision, firmware revision, clock rates, ARM memory size, framebuffer
base and size, and the partition table all belong to the particular board and
card. Only the pass/fail column is portable.

## Recorded hardware result

A physical Raspberry Pi 3 run (11 OK / 0 FAIL, including the SDHCI read of the
real card's MBR) is recorded in
[`docs/HARDWARE-OBSERVATION-AARCH64-SELFTEST.md`](../docs/HARDWARE-OBSERVATION-AARCH64-SELFTEST.md).
That record is the evidence and names its own limits; this README is the
operating procedure.

## QEMU

```
qemu-system-aarch64 -machine raspi3b -m 1G -kernel selftest.elf \
  -drive file=sd.img,format=raw,if=sd -serial file:C:/path/serial.log
```

Use forward slashes in `file:` paths. `file:C:\path` is accepted and silently
writes nothing. Keep the ELF path free of spaces: `Start-Process` drops their
quoting and leaves an empty log.

QEMU cannot answer two things, and the run says so:

- **Cache coherence.** TCG does not model the data caches, so `cache-dirty`
  reports `fail: no-drift` and the demo's uncleaned QR is visible. On real
  silicon the uncleaned QR stays in the cache and the screen keeps the previous
  frame; that is the point of the demonstration.
- **Real firmware facts.** Board revision, clock rates and the ARM memory size
  are QEMU's model of the board, not the board's firmware.
