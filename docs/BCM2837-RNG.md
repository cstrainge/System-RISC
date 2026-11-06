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
| Window size, as the description states it | `0x10` — four 32-bit registers | hardware description |
| Interrupt | 61 (bank 2, bit 29) | hardware description |
| Driver-facing identity | compatible `brcm,bcm2835-rng` | hardware description |
| Window this platform declares | `0x14` — one word wider | **derived by this project** |

The widening is the one fact here that belongs to neither column above, and it is
worth spelling out because the reasoning is ours. The interrupt-mask register
described below is the fifth register, at offset `0x10`, which lies immediately
past the `0x10`-byte extent the hardware description states. A driver that masked
the block's interrupt would therefore write outside the window it was granted.
This platform's record declares `0x14` for that reason, and the reason is written
into the record beside the value rather than left in a driver where the next
reader would have to rediscover it.

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

## The driver

`drivers/bcm2835-rng` supplies that class from this block. It is a package the
System loads by interface, matched to the record's inventory key
`brcm,bcm2835-rng`; nothing about the block enters the kernel or the resident
core.

What it does follows the five rules above, and two of them are the reason it is
written the way it is:

* **It proves the source before reporting ready.** The class says READY completes
  "only after a genuine source completion, never from registration or a
  configured-device lookup alone", so the driver enables the generator and then
  takes a word. A block whose oscillator is dead accepts the enable and counts
  nothing, and is refused instead of reported ready.
* **It refuses rather than fabricating.** Every path that cannot supply a byte
  ends in a refusal naming what it observed — including how many status polls
  produced nothing. A caller seeding a pool cannot tell four zero bytes from four
  bytes of entropy, so zeros are never returned.

Two numbers in the driver are **chosen, not measured**, and are named in its
source so a reader can disagree deliberately: sixteen words discarded after
enabling, and a poll bound at which the source is called silent. Neither is a
claim about timing on this machine.

The interrupt-mask register at `0x10` is why this platform declares a `0x14`-byte
window. The driver writes the mask before the enable, and its build script
asserts the declared window still covers that register, so the record and the
driver cannot drift apart.

**What is still unverified, and it is everything that matters on hardware:** the
block has never been exercised on a board by this project. The eleven host tests
cover the engine's decisions against a simulated register file — that a silent
block is refused, that the count gates every read, that settle words are
discarded — and they establish nothing about whether a real ring oscillator
counts words at the address declared. QEMU's `raspi3b` models no RNG at all, so no
emulator result bears on it either. The list above of what only a board can
settle is unchanged by the driver's existence.
