# The hardware random number generator on BCM2837

The ring-oscillator entropy block at `0x3F104000`, and what a driver may rely on
when using it.

**This document is different from the others in this directory, and the
difference is the point.** Every other hardware specification here can cite a
primary document. This block appears nowhere in the SoC's register document —
there is no chapter, no register table and no description of the interface at
all. Each fact below is therefore labelled with where it comes from:

* **Documented** — stated by the board family's machine-readable hardware
  description, which is a published artefact and can be checked.
* **Observed** — established by use, not stated by any specification. It is the
  hardware's behaviour as far as anyone knows, and it is listed so that a driver
  is reviewable against something explicit rather than against folklore.

Nothing in this document was measured on a board by this project. The one hardware
run recorded in `docs/HARDWARE-OBSERVATION-AARCH64-SELFTEST.md` did not exercise
this block, so the observed facts are inherited, not ours.

## What is documented

| Fact | Value | Source |
| --- | --- | --- |
| Register base | `0x7E104000` (`0x3F104000` in the ARM view) | hardware description |
| Window size | `0x10` — four 32-bit registers | hardware description |
| Interrupt | 61 (bank 2, bit 29) | hardware description |
| Driver-facing identity | compatible `brcm,bcm2835-rng` | hardware description |

The window size matters for a reason worth stating: it covers only the first four
registers. The interrupt-mask register described below is the fifth, at offset
`0x10`, which lies **outside** the documented window. A driver that needs it is
reaching past its declared extent, so this platform's device-tree window is one
register short of what a mask-aware driver would use — a fact recorded here rather
than quietly worked around.

## What is observed

Four registers, 32-bit, little-endian:

| Offset | Register | Meaning |
| --- | --- | --- |
| `0x00` | Control | bit 0 enables the generator; all other bits are not used by any known driver |
| `0x04` | Status | the number of 32-bit words available to read, in the top byte |
| `0x08` | Data | reading it returns one word and consumes it from the FIFO |
| `0x10` | Interrupt mask | bit 0 masks the block's interrupt |

The behaviours a driver has to get right, each of them observed:

* **The generator is off until enabled.** A read of the data register while the
  control bit is clear returns a word that carries no entropy. A driver that
  forgets the enable has a source that always answers and never fails.
* **Availability is counted in words, not bytes.** The count is the top byte of
  the status register; reading data when it is zero is not a wait, it is a read of
  whatever the block returns when it has nothing, which is why the count must be
  checked before every read and re-read after each consumed word.
* **The FIFO is small.** It holds a handful of words — four is the figure in
  circulation — so a request for kilobytes is a loop that drains as the block
  refills, not a single burst. A driver must not assume a fill completes in one
  pass; the interface actually allows a short fill, and this is why.
* **The first words after enabling are not trustworthy.** A ring oscillator needs
  time to settle, and the established practice is to discard an initial batch.
  The number to discard is not specified anywhere; a driver should choose one,
  state it, and be able to demonstrate that it is reading from its own block
  rather than from whatever the block returned during warm-up.

## What a driver must do

1. Enable the block, and wait out a settling period before using any word.
2. Read availability before each word and never read data that is not counted.
3. Return only words the count declared, and report a short fill honestly — the
   entropy interface's contract requires a fill to complete with bytes actually
   supplied, and treats zero bytes as failure rather than success.
4. On quiescence, stop the generator and release the claim. A block left running
   with no owner is a resource leak that a later claimant cannot detect.
5. Refuse rather than fabricate. If the count never rises, the answer is "this
   source produced nothing", not a buffer of zeros — for a caller seeding a pool,
   silently returning zeros is worse than an error.

## What cannot be established from documents

That the enable bit is bit 0, that the count is in the top byte, that the FIFO is
four words, and how many words must be discarded after enabling. Each is stated
above as observed. A driver written to this document is reviewable against it, and
a disagreement between the two is a reason to correct this document — but only a
board run can confirm any of it for this platform, and that run does not exist
yet.

The specification for the driver's interface does have a primary source: the
entropy class (`mac-entropy-interface`), which defines readiness, fill, short
fills and cancellation independently of the device. That contract is what the
resident service validates, and it is not affected by anything unresolved here.
