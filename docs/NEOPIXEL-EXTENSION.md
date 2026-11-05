# An addressable-LED extension: the worked example

A System extension that owns the pins, the serialiser and the DMA channel for one
or more addressable LED strips, and exposes them to applications through a named
endpoint. Applications send colour data and say which pins carry which strips;
they never touch a pin, a PWM register or a timing deadline.

This document is the extension's own contract. It is written against two other
documents and does not repeat them: `docs/TIMED-OUTPUT-CLASS.md` for the
waveform class and `docs/GPIO-CLASS.md` for pin claims.

## Why the split is this way

* **The deadline leaves the application entirely.** An application that toggled a
  pin could not hold a sub-microsecond interval on a preemptive system: the
  thread that would supply the timing is the one that gets descheduled. Here the
  application sends a buffer and the extension turns it into a DMA-fed waveform,
  so the sequence an application runs has no timing requirement at all.
* **Mechanisms belong to the system; a strip is a composition.** The platform
  provides pins, a serialiser and a feeder. "A strip of LEDs with a particular
  protocol on a particular pin" is a composition of those, and that is what an
  extension is for. The kernel knows nothing about LED protocols and must not
  learn.
* **The endpoint's protocol is the extension's contract.** Colour order, the
  strip's layout, refresh and latch semantics, and the errors an application can
  see are all this document's. Only the endpoint mechanism itself is the
  system's.
* **Pin configuration arrives from the application at the right level.** The
  application says which pins carry strips; the extension claims those pins from
  the GPIO class and holds the claims. So the class sees per-pin claims by a
  peripheral-class owner, and the application sees strips. Each abstraction sits
  at its own height.

## What the application sees

One endpoint, published under a name fixed by this extension. Four operations:

| Operation | Meaning |
| --- | --- |
| Describe the strips | Returns the attached strips: their identifier, the pin the application configured, the element count and the colour order in use |
| Configure a strip | Assigns a strip to a pin: pin identity, element count, colour order and the protocol family. The extension claims the pin and the hardware path, or refuses |
| Send colours | A buffer of elements for one strip, in the order the strip was configured with. The extension encodes it and submits a waveform job |
| Refresh and latch | Requests that the last colours sent are emitted and latched; returns when the hardware has finished, not when the buffer was accepted |

Errors an application must be able to tell apart:

* the strip does not exist, or the pin named is not one this board exposes;
* the pin is available but already held — by the system, by another strip, or by
  another application's claim;
* the pin cannot take the serialiser function the strip needs on this board;
* the element count or colour order is not one this extension supports;
* the hardware path is present but busy, which is a retry-later answer;
* a previous frame is still in flight, which is a refusal to *start* rather than a
  queued write;
* the extension has been asked to release the strip, so the application must
  reconfigure it.

A send is not a refresh. Accepting a colour buffer only stores it; nothing appears
on the strip until a refresh emits it and the latch gap has passed. That is
deliberate — it lets an application build a whole frame before anything becomes
visible — and the contract says so rather than letting applications discover it.

## What the extension holds

* One claim per configured pin, in the serialiser function the record declares
  for that pin, held through the GPIO class.
* One serialiser and one DMA channel per active strip, claimed through the
  timed-output class as one exclusive pair.
* The encoded frame buffer for each strip, in storage the feeder can read: a
  waveform buffer the DMA engine cannot reach is a buffer that will glitch.

The extension releases all three on unload, in the reverse order it took them,
and a strip whose pin or channel cannot be claimed is not attached at all.

## Start-up: decline rather than half-attach

The extension declares the interfaces it needs — pin claims, timed output — and
asks for them at initialisation.

* On a board with pins and a serialiser and a feeder, it claims nothing yet: it
  publishes the endpoint and waits for an application to configure a strip. A
  board with the hardware but no configured strip is a working extension.
* On a board without any of them, it receives the class's absence refusal, names
  the platform as the cause, and **declines to attach**. It does not publish an
  endpoint that will fail later, and it does not leave a partially claimed
  hardware path behind.
* On a system whose named-pipe facility is absent — which is every system today —
  it declines for that reason instead, naming the missing facility rather than
  the hardware. That refusal is the same declared-dependency mechanism, and it is
  what makes the extension loadable and reviewable before the transport exists.

The second case is not hypothetical: the emulated ARM board used for development
has no GPIO controller, no PWM and no DMA at all. Loading this extension there
must produce a visible refusal beside the module — not a crash, and not a loaded
extension whose first job fails.

## Dependencies

**Requires named pipes; unnamed pipes exist today but are stubbed, and making
them work properly with named pipes is a separate future project.** This
extension does not build one, and must not: a second pipe implementation inside
an extension is the duplication that makes the real facility harder to land.

What exists today is **class-addressed request/reply**: a module publishes an
interface class, the kernel selects one provider, a caller sends a bounded
request and receives a reply, with a deferred-completion protocol for work that
takes time. Services are resolved by class, not by name. That is enough for the
configuration side of this extension and not enough for the streaming side,
because there is no endpoint an application can connect to by name, no connection
lifetime the extension is told about, and no delivery from the extension to an
application.

The transport being pending does not weaken the design above: the protocol is
independent of how the bytes arrive, and when named pipes land, this extension
becomes wiring rather than a design exercise.

The other dependency — that an application's death closes its connection — is the
pipe project's to provide for the same reason it is not this extension's to
detect: a module cannot safely observe another owner's death.

## What this example does not cover

Colour correction, gamma and brightness are the extension's business if it wants
them; they are not part of the endpoint contract because an application that
pre-corrupts its colours cannot be corrected later. Multi-strip synchronisation
across one refresh is not promised: two strips are two serialisers and two
channels, and they are refreshed in an order the extension chooses. None of this
changes the classes.
