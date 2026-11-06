# SPI0 and the DMA engine as a serialiser and a feeder

The register contract for driving SPI0 from a DMA channel so that a buffer leaves
the shift register at a fixed rate with no CPU instruction between elements. This
is the mechanism behind the timed-output class's first instance.

The class contract is elsewhere (`docs/TIMED-OUTPUT-CLASS.md`); this document is
the hardware: what the two blocks do, what must be true before a transfer starts,
and the ordering rules that make the emitted waveform the one that was computed.

## The two blocks

**SPI0** is a shift register with a divider, a FIFO on each side and three chip
selects. It emits one bit per divider tick and pulls the next word from its
transmit FIFO as it drains. It does not know what the bits mean.

**The DMA engine** moves words from memory into the SPI0 transmit FIFO, paced by
the peripheral's own request line so that the move follows the shift register's
rate rather than the bus's. That pacing is the whole reason a waveform is
possible: without it, the DMA would overrun the FIFO and the contract would
degenerate into software timing again.

## SPI0 registers

Offsets from the window base the record declares (`0x7F204000` in the VideoCore
view, `0x3F204000` in the ARM view).

| Offset | Register | Width | Meaning |
| --- | --- | --- | --- |
| `0x00` | Chip select | 32 | Chip-select polarity and enables, FIFO clear bits, DMA enables, transfer-active |
| `0x04` | FIFO | 32 | Write to push a word into the transmit FIFO, read to pop from the receive FIFO |
| `0x08` | Clock divider | 32 | Divider from the core clock |
| `0x0C` | Data length | 32 | Number of bytes still to transfer, written before a transfer starts |
| `0x10` | Lossi | 32 | Line settings: chip-select timing, input/output mode |
| `0x14` | Status | 32 | FIFO levels, busy, chip-select and DMA states |

The chip-select register's fields that matter here: the transfer-active bit is the
one that starts and stops an emission; the DMA-enable bits let the two FIFOs be
serviced by the engine; the FIFO-clear bits empty a FIFO without ending the
transfer.

**Clock.** The emitted bit rate is the core clock divided by the divider. The
published register document states that the divider must be a power of two; that
statement **does not hold** — any divisor works, and this is one of the document's
recorded errors. A driver is therefore free to choose the divisor that lands
nearest its target rate, and it must, because a power-of-two ladder is far too
coarse for a bit period measured in hundreds of nanoseconds.

What the document does not say, and a driver must assume, is that a divisor of
zero is not a rate: the fastest usable setting is the smallest **nonzero**
divisor, and the arithmetic must be done against the core clock the record
declares rather than the document's nominal 150 MHz, which is not this board's.

**Data length** counts **bytes**, not words and not bits. It is the count the
block decrements as it shifts, and it must be written before the transfer-active
bit is set. Writing it as a word count under-runs the transfer by a factor of four
and the emitted stream is short — which, for a strip, is a truncated frame rather
than an error.

**Status** reports the transmit FIFO's fill state and whether the block is busy.
A driver waiting for completion may watch the busy bit; a driver using DMA should
prefer the engine's own completion, because the FIFO draining is not the last bit
leaving the wire.

## The DMA controller

Fifteen channels, each with a control block in memory holding the transfer
address, the destination, the transfer length and a mode word. A transfer may
chain to a following control block, and the chain is what makes a long buffer one
submission rather than many.

Fields a timed-output transfer depends on:

* **Destination**: the SPI0 FIFO register, with an increment mode that leaves the
  address fixed — every word goes to the same port.
* **Source**: the buffer, with an increment mode that advances one word per word.
* **Transfer length**: bytes, which must be the buffer length in bytes.
* **Pacing**: the destination-request field selects the peripheral whose request
  line throttles the transfer. For SPI0 transmit that is the SPI transmit request;
  selecting none ignores pacing and overruns the FIFO.
* **Width**: a word-wide move, because the FIFO port is 32 bits; a byte-wide move
  would push one byte per transfer and change the rate.
* **Interrupt on completion**: the channel's interrupt is only raised when the
  control block that requests it is the **last in its chain** — a chain that asks
  midway raises nothing. That is one of the published document's errors, recorded
  in the inventory, and it is why completion must be signalled from the final
  block.

**Channel availability.** The board's hardware description gives the ARM a subset
of the channels — channels 0, 2, 4, 5 and 8 through 14 — because the firmware keeps
the others for its own transfers. A driver must take a channel from that set. A
channel the firmware is using will accept a control block and then deliver it
somewhere else entirely, which presents as a corrupted waveform rather than as an
error.

**Addresses.** The engine moves **bus addresses**. A buffer written by the CPU is
not necessarily readable at the address the CPU uses for it, so the buffer must be
allocated through the device service, which returns a device address for exactly
this purpose. Passing a CPU pointer is the classic failure: the transfer succeeds
and the bytes are wrong.

**Caching.** The CPU may hold dirty lines of the buffer when the transfer starts.
Those lines must be cleaned to the point of coherency before the engine reads
them, and the buffer must not be written by the CPU until the transfer completes.
A driver that skips the clean emits whatever was in memory before the buffer was
filled.

## The sequence

1. Claim the SPI0 page and a DMA channel, both exclusive.
2. Configure the SPI block: chip-select polarity and enables, line settings,
   divider, and the DMA enable for the transmit FIFO. Do not set transfer-active
   yet.
3. Allocate the buffer through the device service, fill it with the encoded
   waveform, and clean it to the point of coherency.
4. Write the buffer's length in bytes to the data-length register, then the
   control block's transfer length as the same count.
5. Start the engine, then set the transfer-active bit. Doing it the other way
   round leaves the block waiting on a FIFO that nothing fills.
6. Completion: the engine's interrupt from the last control block, and then the
   block's busy bit cleared. Both, in that order — the engine finishing means the
   last word reached the FIFO, not that it left the wire.
7. Clear transfer-active, clear the FIFOs with nothing selected, release the
   channel and the page.

## Rules an implementation must satisfy

1. Take a DMA channel from the set the board leaves to the ARM, never from the
   whole fifteen.
2. Pass device addresses, never CPU pointers, to the control block.
3. Clean the buffer before starting the engine, and do not touch it until
   completion.
4. Write the data length in bytes.
5. Set transfer-active after starting the engine.
6. Report completion from the last control block's interrupt plus the block's
   busy bit clearing.
7. Never clear a FIFO with a chip select asserted.

## Board checklist for the first run

What a board run should establish, in order, once the aarch64 kernel reaches EL1
and modules execute. Each line is a check with an observation, not a wish:

1. **The page and the channel are claimable** — the SPI0 page is found by its
   `brcm,spi0` key and a DMA channel is obtained from the board's available set.
   Observing a refusal here means the inventory or the channel pool is wrong, not
   the driver.
2. **The divider arithmetic lands** — read back the emitted bit period with a
   logic analyser or by looping the output into another pin sampled by a timer.
   The first thing it catches is a driver that computed against the wrong core
   clock.
3. **The length is bytes** — a buffer of *n* words emits 4*n* bits. A stream that
   ends after a quarter of the frame is this rule violated.
4. **Pacing holds** — no FIFO overrun, no gap between words. An overrun shows as a
   stretched period at a buffer boundary; a gap shows as a longer-than-expected
   frame.
5. **Completion means "off the wire"** — the interrupt, then the busy bit clearing,
   and the last bit present. A driver that stops at the interrupt loses its last
   word.
6. **A second serialiser can be claimed concurrently** — the class's central
   claim. Today this needs the channel arbiter described in the class document;
   without it the check fails for a reason unrelated to this driver.

## What this document does not establish

That the emitted rate is what the divider arithmetic says, that a specific strip
protocol fits the achievable rates, or that the engine and the block agree about
the last word. The emulated machine does not model SPI0's dividers or the DMA
engine's pacing faithfully enough to settle any of those, and no board run has
exercised this path. They are the first things a board run should check, and until
then a driver that compiles and packages is not a driver that has been observed
doing anything.
