# BCM2837 hardware inventory

What a Raspberry Pi 3 model B exposes to software, which of it the ARM cores can
drive, and who owns each piece. This is the inventory a device class and a driver
are chosen from; it is deliberately not a driver description.

Two sources disagree and both are recorded here:

* `platforms/raspi3b.json` is the **declared contract**. The kernel, the
  bootloader and the build all consume it, so a fact only in this document does
  not yet exist as far as the machine is concerned.
* The SoC documentation is the **authority on the silicon**. Where the record and
  the documentation disagree, this document says so and states which is right.

A row marked *record gap* is work for `platforms/raspi3b.json`, not for a driver.

## Addresses

The BCM2837 gives the ARM cores a legacy peripheral aperture at `0x3F000000`.
The VideoCore sees the same registers at `0x7E000000`; the offset from each base
is identical, so one offset serves both. Every address below is the **ARM** view.
The peripheral RAM (the `0x3F000000`-based aperture) is 16 MiB; the ARM-local
block is separate and is not in that aperture.

Offsets are from the base the row names. A window is inclusive of its base and
exclusive of base+size, as the platform record states it.

## Ownership categories

| Category | Meaning |
| --- | --- |
| **ARM** | Registers the ARM cores may read and write. A driver is possible. |
| **VideoCore** | Registers the ARM can address but the VideoCore firmware owns. A bare-metal driver must not reprogram them: the firmware's state and the driver's would diverge silently. |
| **Firmware-mediated** | The ARM reaches the function only through the mailbox property channel, never by touching the block. |
| **Not addressable** | No ARM-visible register interface exists for this function. |

"VideoCore-owned" is not the same as "unusable": the mailbox property channel is
how the ARM asks the firmware to change framebuffer, clock, power and memory
state, and that is a supported interface rather than a bypass.

## Peripherals

Interrupt numbers here come from the board family's own hardware description —
the machine-readable one the reference kernel ships — cross-checked against the
SoC document's interrupt table where the two overlap. That source corrected three
numbers this document previously carried from memory: SPI0 is 54, not 56; all
three I2C controllers share 53, not 55; and PWM and I2S/PCM raise no ARM
interrupt at all. A number written from memory is exactly the kind of fact that
should not reach a record, which is why the record only states the ones this
source confirms.

| Device | Base | Size | Interrupt | Category | Driver class | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| GPIO | `0x3F200000` | 4 KiB | 49, 50, 51, 52 | ARM | **GPIO** (new) | 54 pins in three banks: 0–27, 28–45, 46–53. Bank 0/1/2 have their own interrupt and the fourth line is the "up to 4 pins" acknowledge path. Not in the record. |
| PL011 UART0 | `0x3F201000` | 4 KiB | 57 | ARM | **SERIAL** (PL011) | The console. Declared in the record as `pl011`/`uart`. |
| Mini UART (AUX) | `0x3F215040` | — | 29 | ARM | SERIAL (auxiliary) | Part of the AUX block; shares AUX's enable register and IRQ 29 with SPI1/SPI2. |
| AUX block | `0x3F215000` | 4 KiB | 29 | ARM | auxiliary (container) | Enables mini UART, SPI1 and SPI2 individually. One interrupt line covers all three, so each driver must share and acknowledge. Declared in the record as one 4 KiB window, because all three functions sit in the same page and a claim is per page. |
| SPI0 | `0x3F204000` | `0x200` | 54 | ARM | SPI0 + **timed output** | Master SPI with two chip selects. **Deterministic timed output**: the shift register emits at a divided core clock, so a precomputed buffer becomes edge positions with no CPU in the loop. Declared in the record. |
| SPI1 / SPI2 (AUX) | `0x3F215080` / `0x3F2150C0` | `0x40` each | 29 | ARM | SPI (auxiliary) + timed output | Inside the AUX block, which is declared in the record as one window; a different programming model from SPI0, and they share its interrupt. |
| I2C0 (BSC0) | `0x3F205000` | `0x200` | 53 | ARM | **I2C** (new) | The camera/EEPROM bus. Reserved for the camera connector and the HAT ID EEPROM on some models; the record must state whether it is available here. Not in the record. |
| I2C1 (BSC1) | `0x3F804000` | 4 KiB | 53 | ARM | I2C | The user/GPIO-header bus. All three controllers share one interrupt source on this SoC, so a driver must identify its controller before acknowledging. Not in the record. |
| I2C2 (BSC2) | `0x3F805000` | 4 KiB | 53 | ARM | I2C | Wired to the HDMI DDC lines. Same shared interrupt source. Not in the record. |
| I2S / PCM | `0x3F203000` | `0x24` | **none on the ARM side** | ARM | **timed output** (audio encodings) | A serialiser with its own clock dividers and FIFOs, so it fills the waveform role for audio. **Deterministic timed output.** The board's hardware description gives this block no ARM interrupt, and its reference driver uses none. Declared in the record with `interrupt: null`. |
| PWM | `0x3F20C000` | `0x28` | **none on the ARM side** | ARM | **timed output** + PWM | Two independent channels, each emitting a duty value per period at a divided clock. **Deterministic timed output** — this is the serialiser the LED-strip case uses. The block raises no ARM interrupt at all; completion is a DMA event. The audio output is wired to these channels on the header models, so channel ownership matters as much as controller ownership. Declared in the record with `interrupt: null`. |
| System timer | `0x3F003000` | 4 KiB | 0 | ARM | resident timing | Free-running 1 MHz counter plus four compare channels. Channel 3 is conventionally the ARM's; channels 0–1 are used by the VideoCore firmware's own timers. Declared in the record as `bcm2835-systimer`. |
| ARM timer | `0x3F00B400` | 4 KiB | 64 | ARM | resident timing | A second timer inside the ARM-peripheral area, with its own prescaler and free-running counter. Not in the record. |
| ARM-local block (intc + local timers) | `0x40000000` | 4 KiB | — | ARM | resident interrupt controller | Outside the `0x3F` peripheral aperture. Four cores: per-core control/IRQ-source/FIQ-source registers, per-core mailbox, and the generic timer path. Declared in the record as `bcm2836-l1-intc`. |
| DMA controller | `0x3F007000` | `0xf00` | 16–28 (see note) | ARM | DMA + **timed output** (the feeder) | 15 channels, each with a control block in memory. **A channel is the feeder** that keeps a serialiser supplied without a CPU instruction between elements; it is not itself a serialiser. Channels 0–10 raise interrupts 16–26, channels 11–14 share 27, and 28 is shared across all of them. The board's description gives the ARM channels 0, 2, 4, 5 and 8–14; the firmware keeps the others, so a driver takes a channel from the available set rather than assuming all fifteen. Declared in the record. |
| Mailbox | `0x3F00B000` | 4 KiB | 1 | ARM | resident firmware channel | Four FIFOs (one per ARM core to VideoCore, one VideoCore to all ARMs). The property channel runs inside FIFO 0 and is the only supported way to ask the firmware for clocks, power, framebuffer and memory. Declared in the record as `bcm2835-mbox`. |
| VideoCore framebuffer | (no ARM-visible registers) | — | — | VideoCore | **VIDO** (VideoCore framebuffer driver) | The ARM configures and queries it entirely through mailbox property tags. The scan-out base is chosen by the firmware and cannot be selected by the ARM, which is why a driver must copy into the buffer the firmware reports. |
| HVS (hardware video scaler) | `0x3F400000` | 4 KiB | 65 | VideoCore | — | Composes planes and selects the display composition. The address is not modelled by the emulator at all, and the firmware owns the display list. Should be marked VideoCore-owned in the record rather than merely absent. |
| Pixel valve, scaler, dither, HDMI | `0x3F902000` and the `0x3F800000`-based MMR block | — | 65 | VideoCore | — | The HDMI display pipeline. Only meaningful if the firmware is bypassed for the display, which is not supported here. |
| V3D (3D engine) | `0x3FC00000` | 64 KiB | 66, 67 | VideoCore | — | Three interrupt lines (job done, MMU error, flo). The firmware runs the 3D stack; the ARM has no supported way to submit jobs. |
| TXP (texture pixel processor) | inside the MMR block | — | 65 | VideoCore | — | Legacy 2D path. Superseded by V3D, still owned by the firmware. |
| SMI (secondary memory interface) | `0x3F600000` | 4 KiB | 43, 44 | ARM | SMI | A parallel bus for things like a camera/VGA interface. No consumer on a stock board; listed so nobody wires it by accident. Not in the record. |
| Clock manager (CPRMAN) | `0x3F101000` | `0x2000` | **none** | ARM (read), VideoCore (rate policy) | resident | Divides the oscillators for every peripheral and for the ARM cores, and is therefore what a timed output's element rate is eventually derived from. A driver may set a peripheral's own divider through its control registers, and may query or ask for rates through the mailbox; it must not reprogram the ARM/AXI/display clocks the firmware is using. Two pages, so it is declared as two claimable ranges of one device; it raises no interrupt. |
| Power management (PM) | `0x3F100000` | 4 KiB | — | ARM | resident | Reset controller (offset `0x1C`), watchdog (offset `0x24`), reset-strap status (offset `0x20`). Declared in the record as `bcm2835-pm` and used for the fatal-shutdown path. |
| RNG | `0x3F104000` | 4 KiB | 61 | ARM | **ENTR** (existing class) | A ring-oscillator entropy source. Declared in the record as `bcm2835-rng`; the existing entropy interface has no BCM2837 driver. |
| sdhci (Arasan EMMC) | `0x3F300000` | 4 KiB | 62 | ARM | storage controller | The SD Host Controller Standard Interface, and **the controller this board's card answers on** — see `docs/HARDWARE-OBSERVATION-AARCH64-SELFTEST.md`. Declared in the record as `sdhci`; the boot payload and the shared reader read the card through it. |
| sdhost (SD host) | `0x3F202000` | 4 KiB | 30 (see note) | ARM | storage controller | The older, simpler SD controller. Public descriptions of this SoC family attach the microSD slot of some models to this node and the wireless chip to sdhci; **that does not hold for the board this platform is developed on**, whose card answers over sdhci (`docs/HARDWARE-OBSERVATION-AARCH64-SELFTEST.md`). No driver drives this block, and the record therefore grants no window for it. |
| USB OTG (dwc2) | `0x3F980000` | 4 KiB | 9 | ARM | USB host (future) | The single OTG port. On this board the visible USB sockets are behind an on-board hub chip attached to it, so a driver sees the hub, and a host controller stack is needed before anything downstream is reachable. Not in the record. |
| MPHI (message passing host interface) | `0x3F00B000`-adjacent | — | 32 | ARM | — | Host-interface channels used by the firmware's message protocol. Overlaps the region the record gives the mailbox controller. No driver planned; listed to explain the shared window. |

### Notes on the rows above

* **sdhost's interrupt.** The SD host controller's own interrupt number is not
  used anywhere in this platform today, because the record selects sdhci. The
  number must be resolved from the SoC's interrupt table if the boot medium is
  ever moved to sdhost; a wrong number there would present as a boot that hangs
  with no fault, so it is better left undeclared than guessed.
* **systimer interrupt 0.** The record declares interrupt 0, which is the
  system timer's GPU interrupt, raised through the ARM-local block's basic
  pending register. It is not a GIC-style private interrupt.
* **PL011 at `0x3F201000`.** This is the only UART the board's `enable_uart=1`
  firmware configuration leaves on the GPIO header. It is a full PL011, not the
  mini UART in the AUX block, and the difference matters for FIFO depth,
  fractional baud divisors and interrupt behaviour.

## Deterministic timed output

The peripherals that can be given a buffer and emit it at a rate the driver sets,
without a CPU instruction per element. These are the ones a waveform can be
built on, and they are the resource set the timed-output class claims.

| Peripheral | Role in a waveform | What it emits | Usable as |
| --- | --- | --- | --- |
| PWM | serialiser | a duty value per period, at a divided clock | a symbol's high and low run lengths, by writing one duty value per slot |
| SPI0 | serialiser | a bit stream at a divided clock | a bit's high/low pattern, by choosing a bit rate and a bit pattern per symbol |
| SPI1 / SPI2 (AUX) | serialiser | the same, a separate block | the same |
| I2S / PCM | serialiser | framed audio words at a divided clock | audio, and any framed fixed-rate stream |
| DMA channel | feeder | nothing — it moves a buffer into a serialiser's port | keeping a serialiser supplied between elements |
| Clock manager | rate source | the divided clocks all of the above run from | the origin of the element rate, and thus of the jitter figure |

Not in this list, and not able to do the job: a GPIO pin driven by application
code. The pin can be *the line* the waveform appears on, but the pin's own
control is a level, and a level plus a scheduler deadline is not a waveform.

## What the record is missing

Each of these is a gap in `platforms/raspi3b.json` found by this inventory. They
are recorded here first because a window that no module can claim is a window no
driver may touch: the resident MMIO inventory is what grants a module its claim.

1. **GPIO (`0x3F200000`, 4 KiB, interrupts 49–52).** Required for pin-level
   application access.
2. **Clock manager (`0x3F101000`, 4 KiB).** Required for any driver that has to
   know or set a peripheral clock.
3. **sdhost (`0x3F202000`) — present, unused, and correctly ungranted.** It has
   no `mmio_devices` window, which is right while nothing drives it. If it ever
   gains a driver, it gains the window and the `hardware.devices` entry in the
   same change: a window granted to no module is a claim nobody can use, and a
   driver with no window cannot map its registers.
4. **USB (`0x3F980000`).** Needed only when a USB stack exists.
### What the record gained

`SPI0` (`0x3F204000` + `0x200`, interrupt 54), the `AUX` block
(`0x3F215000` + 4 KiB, interrupt 29, covering mini UART, SPI1 and SPI2), the
`DMA` controller (`0x3F007000` + `0xf00`, channels' interrupts 16–28), `PWM`
(`0x3F20C000` + `0x28`), `I2S/PCM` (`0x3F203000` + `0x24`) and the clock manager
(`0x3F101000` + `0x2000`).

The generated device tree carries a node for each, with the window the record
states — confirmed by parsing the built tree, not by reading the record. A probe
on this platform finds them, and a probe for a driver that does not exist loads
nothing.

Two rules had to change for that, and both changed because the hardware is
broader than the validation was:

* **An interrupt can be declared absent.** PWM and I2S/PCM raise none, so the
  record says `"interrupt": null`. The field is still required: absent is
  refused, so a typo cannot read as "raises none". A window with no interrupt
  reaches consumers as a value outside every interrupt the controller can raise.
* **A device window may span pages.** The clock manager's documented extent is
  two pages; it now claims one page-sized range per page, while the rule that one
  claimed page carries at most one device window is unchanged — that rule is
  about one claimant per page, which the AUX block still obeys.
5. **HVS (`0x3F400000`) and V3D (`0x3FC00000`).** These should be recorded as
   VideoCore-owned so that their absence is a stated decision rather than an
   omission. The resident inventory should be able to express "exists, not
   claimable by a module".
6. **The `hardware.devices` list and `mmio_devices` list describe different
   things and disagree about naming.** `hardware.devices` is the board's physical
   inventory with functional kinds (`uart`, `storage`, `display`, `entropy`,
   `gpio-mailbox`); `mmio_devices` is the claimable register inventory with
   compatible keys. Both are needed, but a device that is present in one and
   absent from the other is currently indistinguishable from a device that does
   not exist. The record should say which of the two lists a new peripheral
   belongs in.

## Device classes this inventory calls for

Classes are the kernel's selection contract: a driver publishes an interface
class, the kernel selects a driver by class and device key, and applications
reach the function through a Toolbox service that the class defines. A class is
defined by its interface crate, never by a driver.

### GPIO — new class

Applications must be able to drive individual pins. That requirement fixes the
shape now:

* The unit of ownership is **one pin**, not one controller. A claim names a pin
  number, the mode it is claimed in (input, output, alternate function *n*), and
  the electrical configuration the claimant requires (pull direction, slew,
  hysteresis). Everything not named stays with its previous owner.
* Two claimants for the same pin are an arbitration decision made by the
  resident owner of the class, not by whichever driver initialised last. A
  refused claim must be distinguishable from a failed one.
* A pin claimed in an alternate-function mode is owned by the peripheral that
  function selects. The class must therefore be able to hand a pin over to, for
  example, the PL011 or SPI0 class rather than treating modes as a GPIO-only
  concept.
* Reads and writes are per-pin and must not require a controller-wide lock that
  a second application can hold.
* The class owns no state that outlives a claim: releasing a pin returns it to
  its prior configuration, because a crashed application must not strand the
  header.

### Timed output — new class, for anything with a deadline

Some peripherals do not just accept a level; they emit a buffer at a rate the
driver sets, which is what makes a waveform possible without the CPU. On this
SoC those are **PWM**, **SPI** (both blocks) and **I2S/PCM**, with a **DMA
channel** as the feeder that keeps a serialiser supplied between elements. The
class over them is described in `docs/TIMED-OUTPUT-CLASS.md`; the reason it is
not the GPIO class is that no application thread can hold a sub-microsecond
deadline on a preemptive system.

A claim is over a serialiser *and* a DMA channel together, because neither
sustains a waveform alone and both are exclusive. A submission is a buffer plus
its element rate and idle level, and completion comes from the hardware. The
addressable-LED strip in that document is the worked case; DMA-fed audio and any
other fixed-rate signalling share the same shape.

### I2C, SPI, PWM — new classes, same granularity rule

* **I2C** claims a *bus and an address*, not the controller: a second application
  may use another address on the same bus. Clock rate is a bus property, so
  changing it requires exclusive bus ownership.
* **SPI** claims a *bus and a chip select*. Two drivers on the same bus with
  different chip selects are legitimate; the bus's clock mode is a negotiated
  property. An SPI block used for timed output is claimed through the timed-output
  class instead, because a waveform occupies the whole shift register.
* **PWM** claims a *channel*, because the two channels are independent and one
  of them may drive audio. A channel emitting a waveform is claimed through the
  timed-output class, which owns it for the duration of a job.

### Existing classes that apply unchanged

* `VIDO` (video output) — the VideoCore framebuffer driver.
* `ENTR` (entropy) — the `bcm2835-rng` driver.
* `VDEV` (claim/DMA service) — every external driver, as today.
* Serial, storage and networking have no class in this tree yet; the PL011 is
  the first one that will need a serial class rather than a private interface.

## Where the published document is wrong

The family's register document covers the BCM2835 and applies to the BCM2837
with the address change above, but it is known to contain errors, and several of
them sit on the path this platform drives. These entries are the hardware's
actual behaviour; the published document disagrees with each.

**Clock.** The document states a nominal core clock of 150 MHz for peripheral
rate calculations. On this board the core clock is 250 MHz. Any rate computed
from the document's figure is wrong by a factor of 1.67, which is the difference
between a UART that prints and one that prints nothing legible.

**Interrupt numbers.** The document's interrupt table is the authority used here,
and it agrees with this record: system timer interrupts are 0 to 3, and 62 is the
Arasan EMMC/SDHCI controller's interrupt — the controller the record calls
`sdhci`, which is the one this board's card answers on.

**Addresses.**
* The UART section prints the PL011's base address with a missing zero. The
  correct base is `0x7E201000`, which is `0x3F201000` in the ARM view.
* The PWM section lists no base address for its register block. It is
  `0x7E20C000`.
* The auxiliary-peripheral register table is wrong for the two SPI blocks: the
  documented offsets do not match the same document's own later pages. The
  correct offsets place the first auxiliary SPI's data port at
  `0x7E2150A0`–`0x7E2150AC` and the second at `0x7E2150E0`–`0x7E2150EC`.
* The same section names those two blocks SPI0 and SPI1. They are **SPI1 and
  SPI2**: SPI0 is a different, separately documented controller.

**GPIO.**
* The alternate-function tables and the pin-function diagrams disagree about
  which selector value selects the SD interface. The register tables say one
  value and the diagrams say another for the same function, and the silicon
  follows neither table uniformly. A pin's function must therefore be taken from
  what the platform record declares, not from either table.
* The pull-up/pull-down registers cannot be read back: a value written to the
  control register is not what a read returns, so a driver cannot verify the
  state it set. Writing one, waiting a bounded number of cycles, clocking the
  selection into the pin and then clearing it is the whole procedure, and the
  required delay is not documented anywhere official.
* The output-clear register is described as setting pins, and the level registers
  are described as writable; both are errors. The clear register clears, and the
  level registers are read-only.
* One register table refers to a third pull clock register that does not exist.
  There are two.

**DMA.** The interrupt-enable field does not behave as described: a channel only
raises its interrupt when the control block that requests it is the last in its
chain. A chain that asks for an interrupt part way through does not get one, so
completion must either be signalled from the final control block or carried by a
second channel.

**I2C.** Clock stretching is not supported at arbitrary points. The controller
tolerates it only directly after the acknowledge phase of a read, and only when
the stretch lasts at least half a clock period. A device that stretches elsewhere
corrupts the transfer, which is a bus-level constraint no class design can paper
over.

**SPI.** The clock divider is documented as needing to be a power of two. It does
not; any value divides. Separately, setting the transmit-FIFO-clear bit while a
chip select is asserted can de-assert that select for a fraction of a clock
period, so a driver must clear FIFOs with no device selected.

**Mini UART.** The interrupt-enable register's two low bits are documented in the
opposite order, and two bits documented as unused are required before the block
raises any interrupt at all. The line-control register needs its second bit set
for eight-bit operation, which the document does not say. The modem status
register's clear-to-send bit is at a different position than documented.

**Random number generator.** The block is absent from the register document
entirely. There is no official register description: the address, the enable bit
and the readiness bit are established by use, not by documentation, so a driver
for it is written against observed behaviour and cannot cite a primary source.

**PL011 interrupt clear register.** The document's table repeats the masked-status
description for two of the clear register's fields where it should describe
clearing. The register clears: writing a one to a field clears that source.

## Not verifiable from this document

The board is the only oracle for anything that is a silicon behaviour rather
than an architectural one: cache maintenance ordering, card-insertion timing,
the pull-up state of a pin at reset, and whether a given firmware call is
accepted in a given power state. Where this document states such a rule, it is
stating what the architecture or the SoC documentation requires, and the
implementation must be reviewable against the rule rather than against a run.
