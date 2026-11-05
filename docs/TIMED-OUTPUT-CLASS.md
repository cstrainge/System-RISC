# Timed-output device class

A contract for handing a waveform to hardware and learning when it finished, so
an application can drive something with a sub-microsecond deadline without
holding the CPU for the duration.

This document defines the class and its timing guarantee. It does not describe a
particular register block; a driver implements it with whatever the SoC provides
for serialising a buffer at a fixed rate.

## Why a pin handle is not enough

An application that toggles a pin from its own thread cannot produce a
sub-microsecond waveform on a preemptive multitasking system, because the
scheduler does not owe it the next 0.4 microseconds. A strip of addressable LEDs
is the clearest case: whether a symbol is a zero or a one is decided by how long
the line is high, and a single descheduled moment inside a frame corrupts it.

That means the requirement is not "a fast toggle primitive" — a faster primitive
is still application code inside a preemptible thread. The requirement is:
**the application hands the waveform to hardware that serialises it without the
CPU's involvement, and asks to be told when it is done.** Everything in this
class follows from that sentence.

## The unit of hardware

Three resources have to come together to emit a timed waveform:

* a **serialiser** — a peripheral that emits one level or one word per clock
  tick of a rate the driver controls (a PWM channel, an SPI shift register, a
  PCM/I2S engine);
* a **feeder** — a DMA channel that moves the buffer into the serialiser as it
  drains, so no CPU instruction stands between two bits;
* a **clock** — a rate from the clock manager that the serially emitted period is
  derived from.

The class's claim is over the *combination*, not over the pin: a serialiser and a
DMA channel claimed together, in the encoding the driver will use. Two of those
resources are exclusive by nature. Two applications cannot share one PWM slice's
duty register or one DMA channel's control block, so a second claim conflicts
visibly rather than interleaving — there is no correct way to interleave two
waveforms on one serialiser.

## What an application does

The class exposes jobs, not pins:

| Operation | Meaning |
| --- | --- |
| Enumerate what can emit a waveform here | Which serialisers, each with its usable rate range, sample widths and event/feeder limits |
| Claim a serialiser for a named encoding | Exclusive; refused if held, naming the holder and the resource |
| Submit a buffer with timing parameters | The hardware starts emitting; the call returns a job identity |
| Ask whether the job finished | Completion comes from the hardware, not from an elapsed-time estimate |
| Cancel | Stops the emission, returns the line to its idle level, and releases the feeder |
| Release | Ends the claim and returns both resources |

A submission carries, at minimum:

* the buffer, in the encoding the claim named;
* the **element rate** — how long each emitted element lasts, which is what the
  driver converts to a divider against the peripheral's clock;
* the **idle level** the line returns to between frames;
* the **inter-frame gap** requested after the last element, where the encoding
  needs one;
* whether the submission may be refused rather than started if the previous one
  is still in flight.

Encodings differ per serialiser and that is the driver's business, not the
application's: a PWM channel emits duty values at a slot rate, an SPI shift
register emits bits at a bit rate. What is identical for every encoding is the
shape of the contract — buffer in, hardware out, completion back.

## The timing guarantee, stated honestly

This class can promise only what the hardware determines, and it must not imply
more:

* **Inside a frame, the period is the peripheral's clock divided by a constant.**
  It is not affected by scheduling: once the job is submitted, no CPU instruction
  is needed per bit. The jitter that remains is the quantisation of that divider
  and the serialiser's own clock, not the operating system's.
* **Between frames, the operating system is in charge.** A preempted application
  delays the *next* submission, so the gap between frames can stretch arbitrarily.
  That is acceptable for an encoding whose protocol only requires a *minimum*
  idle time between frames, and it is not acceptable for one that requires a
  maximum. The class must say which it is providing, and a caller must be able to
  find out that it is a minimum, not a promise.
* **A job that cannot be fed in time glitches.** If the feeder cannot keep the
  serialiser supplied, the emitted waveform stretches rather than failing loudly,
  because the serialiser has no way to go back. The class therefore must refuse a
  submission whose buffer is not somewhere the feeder can read it, and must not
  accept a rate that the claimed resources cannot sustain.
* **Cancellation is not instantaneous.** It stops at a boundary the driver
  chooses and the line returns to idle; the class must state what arrival the
  caller should expect, rather than implying the last element was the last.
* **Nothing here claims a jitter number for a specific board.** A figure belongs
  to a driver, measured on that board, and stated beside the claim. This class
  states only where the figure comes from.

## The worked case: an addressable LED strip

The case that fixes the design is a WS2812-class strip, and it is worth stating
end to end because it rules out several plausible-looking APIs.

**The protocol.** Each LED takes 24 bits — three bytes of intensity — and the
line carries them back to back with no framing, so every bit's duration is
decided by the sender. A bit is a high period followed by a low period: a short
high for a zero, a longer high for a one, with the pair summing to roughly one
and a quarter microseconds. A low period of tens of microseconds ends the frame
and latches it; a longer low is the power-on reset. The exact numbers vary by
part, and the class does not encode them: they are the buffer and the parameters
the application supplies.

**Why application code cannot do it.** The shortest interval in the protocol is
under half a microsecond. A preempted application thread cannot guarantee it, and
the failure is not an error return — it is a strip showing a colour the
application never asked for.

**The hardware paths.** Two are practical on a board with these peripherals, and
both have the same shape: the application precomputes a buffer whose element
durations *are* the symbol timings, and the serialiser emits it at a fixed rate
with DMA feeding it.

* A PWM channel with a DMA channel feeding its duty register. Each LED bit becomes
  a fixed number of PWM slots — the high slots carry the ones of the bit's
  duration and the rest carry zeros — so the emitted period is the PWM period
  times the slots per bit.
* An SPI shift register with a DMA channel feeding its data register. Each LED
  bit becomes a fixed number of SPI bits, chosen so that the run lengths through
  one symbol approximate the high and low durations.

In both cases the application computes bytes; the hardware decides edge
positions. The DMA channel is what makes the buffer arrive without a CPU
instruction between elements — the serialiser asks for the next element, the DMA
supplies it, and the CPU is not in the loop.

**The sequence.** Enumerate the serialisers; claim one for the encoding the
application's buffer is in; submit the frame with its element rate and idle
level; wait for completion; repeat. Between frames the line stays at idle, and
the latch happens because the gap exceeds the part's minimum, which is exactly
the "minimum, not maximum" guarantee above.

**What the application may not do.** It may not hold a pin handle and toggle
it. It may not assume its own loop supplies the timing. It may not assume two
claims of one serialiser both work. It may not assume that a completion means the
strip has updated — completion means the last bit left the serialiser, and the
latch is the idle gap after it.

## Where the composition lives

The class gives an application a job. It does not give an application a strip of
LEDs, and it should not: "a strip" is a composition of a pin, a serialiser, a
feeder, an encoding and a latch rule, and the kernel has no business knowing it.

The shape that works — worked out in full in `docs/NEOPIXEL-EXTENSION.md` — is a
**System extension that owns the hardware and publishes an endpoint**:

* the application sends a colour buffer and says which pins carry which strips;
* the extension claims the pins through the GPIO class and the serialiser and DMA
  channel through this class, and turns the buffer into a waveform job;
* the deadline never reaches the application, and the LED protocol is written
  once, in one place, by whoever knows the strip.

That is the split this class is designed to make possible: mechanisms here,
composition in an extension. An application therefore needs **no** timed-output
claim of its own to drive a strip, and an extension that does the composition
declines to attach on a board with no serialiser rather than publishing an
endpoint that cannot work.

The transport between the two is **pending**: unnamed pipes exist but are stubbed,
and named pipes are a separate future project. That does not weaken the split —
the argument for it is about who holds the deadline, and the deadline is held by
the extension either way — but it does mean the worked example describes a
composition that cannot yet be run end to end.

## Arbitration

* A claim names the serialiser and the feeder. Both are exclusive and both are
  part of the claim, because a serialiser without a feeder cannot sustain a
  waveform and a feeder without its serialiser belongs to an unfinished job.
* The refusal names which resource is held and by whom. "Busy" alone is the
  error-invariant defect this project forbids.
* A job in flight owns both resources until it completes, is cancelled, or its
  owner disappears. An owner that dies mid-job releases them the same way an
  explicit cancel does: the emission stops, the line returns to idle.
* A pin used as the serialiser's output is claimed through the GPIO class in the
  peripheral function the record declares. There is one arbitration point for
  pins, and this class is not a second one.

## Capability, and what a second board changes

The same rules as the other classes apply: capabilities are queried at run time,
the set is extensible, and unknown values are tolerated. For a serialiser that
means its usable rate range, its element widths, whether it can be fed by a DMA
channel and which ones, and how many elements one job may carry.

A board with different peripherals — a different PWM slice count, an I2S engine
where the other board had none, a DMA controller with different channel
semantics — changes the record and, where the register model differs, the driver.
It does not change this class and it does not change an application that
feature-detects. In particular an application must be able to ask "is there any
serialiser here that can emit this encoding at this rate?" and receive a truthful
"no" naming what exists instead.

## Testability without hardware

The mapping and the capability data being record-supplied makes the class
testable before a board is powered: two records with different serialiser sets
must show the same application request accepted on one and refused, with a named
reason, on the other; and a submission must be shown to describe the buffer, the
rate and the idle level to the driver rather than to assume any of them.

These cases establish the contract, not the emission. Whether the hardware
emits the waveform the spec describes — the edge placement, the divider
quantisation, the behaviour at the maximum rate — is a board fact, and a driver
claiming it must say on which board it was measured.
