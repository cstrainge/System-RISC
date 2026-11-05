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

The second case is not hypothetical: the emulated ARM board used for development
has no GPIO controller, no PWM and no DMA at all. Loading this extension there
must produce a visible refusal beside the module — not a crash, and not a loaded
extension whose first job fails.

## What this example needs from the system, and does not have

**There is no named endpoint that an application can connect to.** Stated plainly
because it is the one part of this design that does not exist:

* What exists is a **class-addressed request/reply**: a module publishes an
  interface class, the kernel selects one provider for it, and a caller sends a
  request and receives a reply with bounded buffer sizes. That is a good
  mechanism for "ask the device a question", and it is what every driver in this
  tree uses.
* What is missing is a **named endpoint with a connection lifetime**: an
  application looks up a name, connects, exchanges several messages, and
  disconnects — with the extension learning of the connect and the disconnect,
  including the disconnect caused by an application dying.
* Also missing is **delivery in the other direction**: today a caller asks and a
  provider answers. An endpoint that must tell an application about a refused
  refresh or a released strip needs the provider to initiate.

A one-off pipe inside this extension would be the wrong answer, for three
reasons that are reasons of correctness rather than taste:

1. **Noticing that an application has gone** is the kernel's job. A module cannot
   safely observe another owner's death, so a per-extension pipe would either leak
   its connection or trust an application to disconnect cleanly.
2. **Queueing between two fault domains** needs bounded storage with an owner, a
   backpressure rule and a teardown order. Every extension that streams would
   need those rules again, and they would diverge.
3. **The kernel boundary already exists.** An application cannot call a module
   directly; a pipe has to be a resident service that owns the queue and relays
   to the extension. Building it inside the extension would mean building a
   second boundary beside the real one.

**Recommendation: build the facility once, in the system, and scope this example
against it.** The shape it needs:

* a name registry: a provider publishes a name, a consumer resolves one, and a
  duplicate name is a refusal rather than a second endpoint;
* a connection object with a lifetime: connect, messages both ways, disconnect,
  and a disconnect the provider is told about — including one caused by the
  consumer's owner disappearing;
* per-connection bounded queueing with an explicit full-queue policy, because a
  consumer that stops reading must not consume kernel memory;
* exactly the same rules for an application-facing consumer and an
  extension-facing one, for the same reason the GPIO class has one contract and
  two presentations: two implementations would disagree invisibly.

Until that exists, this extension can be written and reviewed against the classes
it does use, but it cannot be run end to end: there is nothing for an application
to connect to. That is a gap in the system, not a defect in this design, and it
is the reusable half of the pattern — the next extension that wants to stream
anything will need the same thing.

## What this example does not cover

Colour correction, gamma and brightness are the extension's business if it wants
them; they are not part of the endpoint contract because an application that
pre-corrupts its colours cannot be corrected later. Multi-strip synchronisation
across one refresh is not promised: two strips are two serialisers and two
channels, and they are refreshed in an order the extension chooses. None of this
changes the classes.
