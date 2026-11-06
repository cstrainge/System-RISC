# Hardware observation: AArch64 self-test on a Raspberry Pi 3

**This is an observation record, not a specification.** It records what one
board did with one binary. It does not define how storage is wired, and it does
not replace the platform record or the SDHCI register documentation.

## The harness, and why its paths no longer resolve

The program that produced these observations was throwaway scaffolding for
board bring-up: it brought up UART and the mailbox framebuffer, ran a rotation
of cache, MMU, DMA and SPI0 checks, and encoded the results into a QR on the
panel. It has been removed from the tree — that work was scaffolding, not a
product. It remains in history at `e54b4e21` under `selftest-aarch64/`, so the
paths quoted below resolve there and nowhere else.

The observations are the durable part and stand on their own. Re-running any of
them would mean rebuilding that harness first.

## What was observed

- **Board:** a physical Raspberry Pi 3 (BCM2837-class SoC), card in the slot,
  display attached. The board's own firmware performed first-stage boot.
- **Image:** the flat `kernel8.img` built from commit `412ba4d9` of branch
  `cstrainge-aarch64-hardware-self-test`, the last commit of the board bring-up
  session. It was produced from
  `selftest-aarch64/target/aarch64-unknown-none/release/selftest-aarch64` with
  `rust-objcopy -O binary`. The firmware loaded it raw at `0x80000`; it is not
  an ELF. The exact flashed file and its hash are not preserved in this
  repository.
- **Firmware configuration:** the self-test `config.txt` variant, which enables
  `dtoverlay=disable-bt` (console on PL011 rather than Bluetooth), removes
  `dtoverlay=vc4-kms-v3d` and `disable_fw_kms_setup=1`, and disables
  `auto_initramfs`. The firmware handover pointer in `x0` was ignored; no device
  tree or command line was parsed.
- **Observed:** the self-test's results screen on the board's display. The
  eleven checks all reported `ok`: **11 passed, 0 failed**.

## Provenance of the values below

The run's output was **photographed from the physical display and decoded after
the fact**. No serial log was preserved, no `SELFTEST_RESULT` line survives as a
file in this repository, and no copy of the flashed image was kept. The values
in this record are transcribed from those photographs.

Only the SDHCI detail fields were transcribed field by field, because they are
the evidence for the controller claim. The other checks' numeric details (board
revision, firmware revision, clock rate, ARM memory size, framebuffer base,
size and pitch, timer delta) are **not recorded here** rather than guessed. A
future lane that needs them must re-run the binary with a serial capture.

## The controller under test

The self-test called `Disk::probe(SDHCI_BASE, ...)` and then read sector zero.
`SDHCI_BASE` is the base of the platform record's storage device:

```
platforms/raspi3b.json
  hardware.devices[kind = "storage", model = "sdhci"].base = 0x3f300000
```

No address appears in the reader's source; it is passed in from that record. At
the time of the run the reader lived in `selftest-aarch64/src/sdhci.rs`; it has
since moved to `crates/mac-sdhci` with the same transfer path. The parts of that
path that this observation depends on, and that the board corrected:

- `PRESENT_BUFFER_READ` is Present State bit 11, *Data Buffer Read Enable*. Bit 9
  is *Data Transfer Active* and is set before any byte has arrived; draining on
  it returns a block of zeroes.
- CMD17 is issued as a single block. The multi-block select bit is clear; with
  Auto CMD12 disabled it leaves the card mid-transfer after one block.
- The data port is drained only after the Buffer Read Ready interrupt or the
  data-available bit, and a transfer that completes without either is reported
  as a different fault from a short block.
- CRC and command-index checks are enabled per response type: R3 carries no CRC
  and R2 carries no command index.

QEMU does not distinguish any of these: its model has the data available
immediately and validates neither check.

## Result: SDHCI sector zero

The card's master boot record was read through the Arasan controller at
`0x3f300000`:

| Field | Value |
|---|---|
| MBR signature | `0x55AA` |
| Partition 0, first LBA | `16384` |
| Partition 0, type byte | `0x0C` (FAT32 LBA) |
| Partition 1, first LBA | `1064960` |
| Partition 1, type byte | `0x83` (Linux) |

The values were read from the results screen's SDHCI detail line; the signature
and both entries were legible. The results screen also reported `sdhci=ok` in
the QR text.

### Why these values matter

The built test image's own partition table starts its partitions at `2048` and
`526336`. The card's starts are `16384` and `1064960`. The read therefore
returned the **user's actual microSD card**, not bytes the self-test or its
image builder wrote. That is what makes this evidence about the controller
rather than about our own fixture.

## What this establishes

- The Arasan SDHCI controller the platform record drives, at `0x3f300000`, can
  identify and read the real microSD card on this Raspberry Pi 3. This directly
  contradicts the conclusion that the card cannot be read on real silicon with
  that controller.
- The mailbox and VideoCore framebuffer path works on real firmware: the results
  screen and its QR were produced on the board's display, not in emulation.
- The cache experiment behaved as designed in both directions: the cleaned
  read-back passed, and the uncleaned write was observed through the test page's
  non-cacheable alias as the earlier pattern (`cache-clean=ok`,
  `cache-dirty=ok`). The QEMU run of the same binary reports
  `cache-dirty=fail: no-drift` because TCG does not model the data caches.

## What this does not establish

- It says nothing about `native.rs`, the kernel handoff, or any driver beyond
  this boot-time reader.
- It does not show that `bcm2835-sdhost` is absent, broken, or unnecessary. It
  shows that the Arasan SDHCI path works, so a replacement controller is not a
  prerequisite for reading the card on this board.
- It does not exercise DMA, interrupts, multi-block transfers, write support,
  error recovery, or performance. The reader is polled single-block PIO and
  never writes to the card.
- It is one board, one card and one firmware configuration. It does not
  generalise to other Pi revisions, cards, or `config.txt` variants.
- It is not a preserved log. The source is a photograph, and only the SDHCI
  fields above were transcribed. There is no artifact in the repository from
  which the rest can be re-derived.
- It says nothing about the SPI0, DMA or timed-output checks, or the additional
  display checks, which postdate this run and have not been executed on
  hardware. See the next section.

## Checks added after this run (not observed on hardware)

Two more groups of checks were written after the board run recorded above, so
this record does not claim them: eight SPI0 and DMA checks, and the timed-output
checks that the SPI0 driver is built on. They are `spi-claim`, `spi-len`,
`spi-div`, `spi-pace`, `spi-done`, `dma-lease`, `dma-2nd` and `dma-clean`. What
follows is what the code does, which is not the same as what a board has
confirmed.

- No external wiring is required. The checks use the SPI0 FIFO, the DMA engine
  and RAM. The byte-length check counts receive words, so MISO may float. A
  transmitted-versus-received data comparison would need a jumper from GPIO10
  (MOSI, pin 19) to GPIO9 (MISO, pin 21); the test does not configure the GPIO
  alternate functions, so an attached strip or analyser sees nothing on the
  header even while the block shifts.
- An LED strip is not required. If one is attached, the test emits arbitrary
  bytes to it; that is harmless and not part of the result.
- The SPI0, DMA, power-management, PWM, I2S and clock-manager windows are
  declared in `platforms/raspi3b.json`, so a driver takes them from the record
  rather than from a constant. They were added to the record after this run, and
  nothing in this record is evidence about them.

## Reproducing

Build and card procedure are in
[`selftest-aarch64/README.md`](../selftest-aarch64/README.md). Under QEMU the
same binary boots with `-machine raspi3b -m 1G`; the SDHCI check needs a card
image attached with `-drive ...,if=sd`, and the cache checks cannot be answered
there.
