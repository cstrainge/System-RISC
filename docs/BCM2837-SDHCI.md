# SDHCI host controller on BCM2837

The register contract for the Arasan SD Host Controller Interface block at
`0x3F300000`, as the boot payload drives it to read a card block by block.

This is the controller's own specification. Where a rule here differs from what
an emulated machine does, the rule is what the silicon requires and the emulator
is what it is: `-machine raspi3b` answers some status bits before any data has
moved, so a check that depends on waiting can pass there without ever exercising
the wait.

## Which controller owns the card

On this board the microSD slot is wired to **sdhost** (`0x3F202000`), and this
controller — the Arasan EMMC/SDHCI block — carries the wireless SDIO interface.
The distinction matters more than any register in it: a driver written against
this block does not see the card on real silicon, however correct its register
use is. This document specifies the controller because the platform's emulated
machine offers no other one, and because the block is a legitimate device to
drive for its real function. The inventory records the gap.

## Where this contract comes from

The SoC's own register document does not describe this block. Its only mention is
in the interrupt table, where it names the controller and assigns it interrupt 62
— a fact this platform's record repeats, and the only part of the block the SoC
document corroborates. Every register, bit and ordering rule below comes from the
SD Host Controller interface specification the block implements, which is why
the block is described here as an implementation of that interface rather than as
a Broadcom invention.

One consequence: a discrepancy between the two would be resolved in the
interface specification's favour for anything that interface defines, and in the
SoC document's favour for which interrupt the block raises.

## Register map

All access is little-endian. Registers narrower than 32 bits are accessed at
their own width; the transfer-mode and command registers are adjacent and are
written together as one 32-bit store.

| Offset | Width | Name |
| --- | --- | --- |
| `0x00` | 32 | System address (DMA only) |
| `0x04` | 16 | Block size |
| `0x06` | 16 | Block count |
| `0x08` | 32 | Argument |
| `0x0C` | 16 | Transfer mode |
| `0x0E` | 16 | Command |
| `0x10`–`0x1F` | 32 each | Response 0 to response 3 |
| `0x20` | 32 | Buffer data port |
| `0x24` | 32 | Present state (read-only) |
| `0x28` | 8 | Host control 1 |
| `0x29` | 8 | Power control |
| `0x2A` | 8 | Block gap control |
| `0x2C` | 16 | Clock control |
| `0x2E` | 8 | Timeout control |
| `0x2F` | 8 | Software reset |
| `0x30` | 16 | Normal interrupt status |
| `0x32` | 16 | Error interrupt status |
| `0x34` | 16 | Normal interrupt status enable |
| `0x36` | 16 | Error interrupt status enable |
| `0x38` | 16 | Normal interrupt signal enable |
| `0x3A` | 16 | Error interrupt signal enable |
| `0x3C` | 16 | Auto CMD error status |
| `0x40` | 32 | Capabilities |

## Present state

Read-only, and the only authority on whether a transfer is ready for the next
step.

| Bit | Meaning |
| --- | --- |
| 0 | Command inhibit: an outstanding command owns the command line |
| 1 | Data inhibit: an outstanding transfer owns the data lines |
| 2 | Data line active |
| 8 | Write transfer active |
| 9 | Read transfer active |
| 10 | Buffer write enable: the host may push data into the buffer |
| 11 | Buffer read enable: the host may pull data from the buffer |
| 16 | Card inserted |
| 17 | Card stable |
| 18 | Card detect pin level |
| 19 | Write protect pin level |
| 24 | Command line signal level |

**Bits 9 and 11 are different facts.** Bit 9 says a read transfer is in
progress; bit 11 says a word is waiting in the buffer port to be read. A host
that waits on bit 9 before reading the port reads nothing on silicon, while a
host that waits on bit 11 reads as soon as data arrives. This is the single most
likely place for a driver to look correct and be wrong: the emulator can present
data and complete the transfer in the same instant, so a host that confuses the
two still returns the right bytes there.

## Interrupt status and enables

Normal interrupt status bits: 0 command complete, 1 transfer complete, 2 block
gap event, 3 DMA interrupt, 4 buffer write ready, 5 buffer read ready, 6 card
insertion, 7 card removal, 8 card interrupt, 12 retuning event, 15 error
interrupt.

Error interrupt status bits: 0 command timeout, 1 command CRC, 2 command end bit,
3 command index, 4 data timeout, 5 data CRC, 6 data end bit, 7 current limit,
8 auto CMD12, 9 ADMA, 10 tuning.

Two rules govern all of them:

* **A status bit only latches if its enable bit is set.** A host that polls a
  status bit without setting the corresponding enable polls a bit that never
  becomes true.
* **Both status registers are write-one-to-clear.** Writing zeroes changes
  nothing, so a host that clears by writing zero clears nothing; clearing by
  writing all ones clears every pending condition, which is only correct when
  the host is discarding the transfer anyway.

Bit 15 of normal interrupt status is not a condition of its own: it is the
logical OR of the masked error interrupt status, and it is what a polling host
checks to detect failure cheaply.

## Command register

| Bits | Field |
| --- | --- |
| 1:0 | Response type select: `00` none, `01` 136-bit, `10` 48-bit, `11` 48-bit with busy |
| 2 | Command CRC check enable |
| 3 | Command index check enable |
| 4 | Data present select |
| 5 | Command type (suspend/resume/abort) |
| 13:8 | Command index |

The CRC and index checks are **per-response-type facts, not per-host settings**.
A response that carries no CRC (the operating-conditions response used during
card identification) must not have the CRC check enabled: the controller checks a
field that does not exist and reports a CRC error for a command that succeeded.
Enabling the CRC check for every command that has a response is therefore a
defect that only shows up on silicon, because an emulator may return success
regardless.

The index check applies only to responses that carry the command index.

## Transfer mode and the data port

| Bits | Field |
| --- | --- |
| 0 | DMA enable |
| 1 | Block count enable |
| 3:2 | Auto CMD12 or auto CMD23 enable |
| 4 | Data transfer direction, 1 for card-to-host |
| 5 | Multi-block select (reserved on this generation) |

For a single-block read the host sets the direction, enables the block count and
leaves multi-block select clear. Bit 5 is reserved on controllers of this
generation; a host that sets it is writing an undefined field.

The block size and block count registers must be programmed before the command is
issued, and the transfer mode and command registers are written in one store
because the command is issued by the store that contains both.

The buffer port may only be read while present state bit 11 is set. Every read
takes one 32-bit word; a block of 512 bytes is 128 reads. The transfer is
complete when present state bit 9 clears and normal interrupt status bit 1 is
set; the next command must not be issued until then, or the data inhibit bit
keeps it from being accepted.

## Clock control

Bit 0 enables the internal clock, bit 1 reports it stable, bit 2 enables the SD
clock, and the divider is ten bits split as bits 7:6 (high two) and 15:8 (low
eight).

`SDCLK = base_clock / (2 * N)` for `N >= 1`; `N = 0` is not used, because its
meaning depends on the controller generation. The base clock is reported in
capabilities bits 15:8, in whole megahertz, with zero meaning the part's default.

Ordering: the internal clock must be enabled and observed stable **before** the
SD clock is enabled. A card given a clock that is not yet stable does not answer,
and the failure appears as a command timeout rather than as a clock problem.

## Power and reset

Power control bit 0 switches bus power on; bits 3:1 select the voltage, with `111`
selecting 3.3 V, which is what an SD card expects. Bits 7:4 are reserved.

Software reset bits: 0 reset all, 1 reset the command line, 2 reset the data
line. The reset is complete when the written bit reads back as zero; nothing may
be issued before that.

## Card identification

The sequence is fixed by the card's own specification and each step's response
type is fixed with it:

1. **CMD0** — no response — puts the card in idle.
2. **CMD8** with the 2.7–3.6 V pattern — 48-bit response with CRC — discovers
   whether the card accepts the high-capacity protocol.
3. **CMD55 then ACMD41**, repeated until the card reports ready — 48-bit
   response, **no CRC** — with the capacity bit set and the voltage window
   offered. The argument is only meaningful on the first ACMD41 after CMD8.
4. **CMD2** — 136-bit response with CRC — returns the card identity.
5. **CMD3** — 48-bit response with CRC — assigns the relative address.
6. **CMD9** — 136-bit response with CRC — returns the card-specific data, from
   which capacity is computed.
7. **CMD7** — 48-bit response with CRC, busy after the response — selects the
   card and takes it out of stand-by.
8. **CMD16** — 48-bit response with CRC — sets the block length to 512 bytes for
   cards that are not high capacity.

A 136-bit response is presented with its most significant word last: the first
response register holds the least significant 32 bits, and the card identity's
structure field therefore lives in the top two bits of the fourth response
register.

Capacity from card-specific data:

* **Structure 1 (high capacity)** — capacity is reported in 512 KiB units as a
  22-bit field, so the sector count is `(field + 1) * 1024`.
* **Structure 0 (standard capacity)** — capacity is
  `(C_SIZE + 1) * 2^(C_SIZE_MULT + 2) * 2^READ_BL_LEN` bytes, converted to
  sectors by dividing by 512.
* Structures 2 and 3 are not SD memory cards and have no capacity this reader
  can use; refusing them is correct because addressing them as a block device
  would produce reads of whatever the card interprets those fields as.

## Rules an implementation must satisfy

1. Read the buffer port only while present state bit 11 is set, and treat bit 9
   as "a transfer is in progress", never as "data is available".
2. Enable the CRC check only for response types that carry a CRC, and the index
   check only for responses that carry an index.
3. Set the interrupt status enables for every status bit the host polls.
4. Clear status by writing ones to the bits being cleared.
5. Wait for transfer complete before issuing the next command.
6. Enable and observe the internal clock stable before enabling the SD clock.
7. Write the transfer mode and command registers as one store.
8. Leave multi-block select clear for a single-block transfer.
9. Bound every poll: a card that never answers must fail the read, not hang the
   boot.

## What this document does not establish

Nothing here proves the controller works on the board. The data-available rule
and the CRC-check rule are exactly the two that an emulated machine cannot
contradict, so an implementation that follows this document is still unverified
against silicon until it reads a card on one. Conversely, a run that succeeds in
the emulated machine establishes only that the bytes arrived there.
