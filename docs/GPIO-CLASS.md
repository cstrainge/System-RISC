# GPIO device class

A contract for claiming and driving individual GPIO pins from applications, so
that one application works on every supported board without a per-board build and
without knowing which SoC is underneath.

This document defines the class: what a pin is, who may own it, how a request is
answered, and what a platform record must supply. It is not a register
description. A driver implements this class against a particular SoC's GPIO
block; the board's pin mapping is data.

## Why the class exists

Three requirements decide its shape, and each of them rules out an alternative
design that would be cheaper now:

1. **Applications drive pins.** Something that a person wires to a header is
   application-visible. A class that lets one driver own a whole controller
   cannot express "this application uses pin 11 and that one uses pin 12", and
   retrofitting per-pin ownership later changes the interface every application
   already uses.
2. **A pin is a board fact, not an SoC fact.** The header pin a person wires is
   the same pin on every model that has that header. The SoC pin behind it is
   frequently not: models differ in which SoC pin the header position reaches,
   and they differ in which alternate functions those SoC pins can take.
3. **Many boards, one driver rule.** A model-specific branch in a driver is how a
   platform that supports two models ends up supporting neither well. The
   mapping must be data so a new model is a new record.

## Definitions

* **Logical pin** — the board's stable identity for a pin: the thing a person can
  see on the board and wire. It is a name chosen by the platform record, and it
  is the only pin identity an application ever uses. Its meaning is "the pin this
  board exposes at this position", which is stable across models sharing the
  board layout.
* **SoC pin** — the number the SoC's own GPIO block uses for that pin. It is a
  record detail and never appears in an application's request.
* **Function** — what the pin is doing: a plain input, a plain output, or a named
  peripheral signal (a UART transmit line, an SPI clock, a PWM channel output).
  Functions are named by what they carry, not by the number of an alternate
  function selector, because those numbers differ between SoCs.
* **Mode** — the combination of function and electrical configuration a claim
  holds: function, pull direction, and the output drive and input characteristics
  the board declares for that pin.
* **Class owner** — the resident service that arbitrates claims. It is not the
  driver and not an application. It owns the arbitration state so that a driver
  restart or an application's exit does not lose track of who holds a pin.

## Pin identity

An application addresses a pin by its logical pin name, obtained by enumerating
the board's pins rather than by compiling a constant. Enumeration returns, for
each logical pin, the pin's name, its capabilities (below), and whether it is
currently claimable. An application that hardcodes a logical name is relying on
the board layout, which is legitimate; an application that hardcodes a SoC pin
number is not, and the class provides no way to express one.

Two consequences are deliberate:

* A board that does not expose a pin at all does not enumerate it, so an
  application can ask "does this board have this pin?" without a version check.
* A board that exposes the pin but not the function the application wants
  enumerates the pin and refuses the claim with a typed reason. "Not present" and
  "present but incapable" are different answers and must stay distinguishable.

## The record supplies the mapping

The platform record carries, for every logical pin:

* the SoC pin it is on this board;
* the functions that SoC pin can take **on this SoC**, keyed by function name;
* the electrical capabilities available on that pin: pull-up, pull-down,
  output drive settings, input hysteresis, and whether the pin can generate an
  event on an edge, on a level, or both;
* any restriction the board imposes that the SoC does not, such as a pin the
  board's own hardware already uses or a pin that is not routed to the header.

The driver contains none of this. There is no pin table in the driver, no board
name test, and no model comparison. The driver knows only how to make a SoC GPIO
block produce a requested mode on a requested SoC pin, and how to report the
state of one SoC pin. Everything between "logical pin" and "SoC pin" is the
record's job.

A capability that the record does not declare for a pin is **absent**, not
"probably fine": the class refuses the request. Writing a selector value that the
record does not name would put the pin into whichever mode that value happens to
select on this silicon, which is how a board ends up driving a signal nobody
asked for.

## Ownership

The unit of ownership is **one logical pin**, in **one mode**, held by **one
owner**. Owning a pin is not owning the controller, and no operation on one pin
may block or perturb another pin's owner.

* A claim names one logical pin, one function, and the electrical configuration
  the claimant wants. The claim succeeds only if every part of it is declared
  possible for that pin on this board and the pin is not already held.
* A claim is attributed to the requesting owner (the application's own
  identity, or a class owner for a peripheral driver). Refusing a claim because
  another owner holds the pin is a different outcome from refusing it because
  the pin cannot do the requested function, and the two must be reportable
  separately.
* Release is per pin and returns the pin to the configuration it had before the
  claim. An owner that disappears — crashes, exits, is killed — has its claims
  released by the class owner; a crashed application must not leave the header
  in a state the next application cannot recover.
* A pin that has never been claimed has the state the board's reset leaves it in,
  and the class must report that state rather than a fabricated default.

## Handover between classes

A pin claimed in a peripheral function is held by that peripheral's class, not by
the application that uses the peripheral. The peripheral class claims the pin
through the GPIO class, which is the single arbitration point: there is no second
place that can grant the same pin.

That means the GPIO class must accept a claim on behalf of another class rather
than only on behalf of an application, and it must refuse an application's claim
that would take a pin a peripheral class holds. It also means a peripheral
driver's own claim failure is a first-class error at start-up: the peripheral must
report "the pin I need is taken", not fail silently.

## Events

A pin claimed as an input may ask for events on an edge, a level, or both. Events
are delivered to the pin's owner only. The ordering rules:

* An event is attributed to a logical pin, not to the controller's interrupt
  line. A shared interrupt line is the driver's problem, not the owner's.
* Acknowledging one pin's event must not discard another pin's pending event.
  Any design that clears a whole bank to acknowledge one pin loses events and is
  not acceptable.
* Event delivery must be possible without the owner polling, and must not require
  the owner to hold the class's arbitration state.
* The board may not route a pin's events at all; the record says so, and the
  claim for edge events on such a pin is refused rather than silently ignoring
  the edge.

## Refusal

Every refusal is typed and distinguishable from every other. The minimum set:

* the logical pin does not exist on this board;
* the pin exists but the requested function is not available on it on this board;
* the pin exists and the function is available, but another owner holds it;
* the electrical configuration requested is not available on this pin;
* the owner asked for an event mode the pin cannot produce;
* the class owner itself is unavailable (for example during a driver restart),
  which must not be reported as "pin busy" because it is not an ownership
  conflict.

An application must be able to distinguish "retry later" from "this board cannot
do that" without reading a board name.

## Consistency across models

The class's promise is: **an application written against one board's logical
pins runs unchanged on another board that exposes the same layout.** It does not
promise that the same SoC pins are behind them, and it does not promise that every
function is available on both.

The worked example that fixes the design:

* BCM2837 places its peripherals at `0x3F000000` and has one GPIO block layout.
  BCM2711 places them at `0xFE000000` and has a different GPIO block layout, with
  different register spacing and some functions moved to different pins or
  dropped.
* "The header pin is the same" is therefore a true statement about the board and
  a **false** statement about the SoC pin. An application asking for the pin at
  the same logical position is asking the right question; a driver that assumed
  the same SoC pin, or the same alternate-function number, would silently drive
  the wrong signal.

What a second Pi model requires under this design:

1. a new platform record carrying that board's pin mapping, function names and
   capabilities;
2. if the SoC's GPIO block differs in layout, a driver for that block — a second
   instance of the same class, selected by the record's device identity, not a
   branch inside the first driver;
3. **no change to any application and no change to the class.**

If supporting a second model needs anything else — a new field in the class's
request that only one model uses, or a code path that asks which board this is —
the design is wrong and the class must be corrected before the second record is
written.

## Testability without hardware

The mapping being data is what makes the class testable before any board is
powered:

* Two records with deliberately different mappings — the same logical pin on
  different SoC pins, the same function name on different pins, one function
  present on one record and absent on the other — must be exercised through the
  same driver.
* The expected outcomes are: identical logical requests succeed on both where the
  capability exists; the absent-function request is refused with the "not
  available on this board" reason; the SoC pin never appears in an application's
  request or in a refusal message.
* A third record that omits a logical pin entirely must show the pin as
  non-existent rather than unsupported.

These are record-level cases. They do not establish anything about the register
writes a real SoC performs, which requires the board.
