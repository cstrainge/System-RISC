# VideoCore IV display and 2D interface, BCM2837-class platform

Status: design input, not an implementation plan. This document describes the
hardware interface the BCM2837-class platform exposes to the ARM cores, states
which parts of it this OS can use, and records the conflict between what the
primary documentation says and what working software actually does. It contains
no code, no pseudo-code and no proposed data structures; those are designed from
this description at implementation time.

Scope is the `qemu_arm64` platform (`platforms/qemu_arm64.json`, QEMU machine
`raspi3b`) and the physical part it models: a BCM2837-class SoC with four
Cortex-A53 cores, 1 GiB of RAM and a VideoCore IV VPU. The question it answers
is how much of QuickDraw's work can be moved off the CPU, with the minimum
acceptable outcome being hardware composition of the desktop rather than
software composition into one linear framebuffer.

---

## 1. Method, sources and the three tiers

Two classes of source are used, and they are not interchangeable:

* **Primary documentation** establishes the model: what a block is, where its
  registers live, what a field means. Sources are the *BCM2835 ARM Peripherals*
  datasheet (06 February 2012) and the *VideoCore IV 3D Architecture Reference
  Guide* (`VideoCoreIV-AG100-R`, 16 September 2013).
* **The mainline `vc4` DRM driver** establishes reality: which fields are
  actually written, in what order, and which documented features are never
  used. It has run on this silicon for about a decade. It is cited for facts
  only; no code, identifier, comment or control flow is reproduced, and the
  architecture in section 12 is derived from this project's own module model,
  not from DRM. Where a hardware fact is recorded only by that driver, it is
  stated here in prose with the file cited, rather than transcribed.

Where the two disagree the disagreement is recorded as a numbered conflict
(sections 3 and 5.4). Silence in the datasheet is itself a finding: the
datasheet states in §1.1 that GPU-controlled peripherals are omitted from it,
and that is exactly the set this design needs.

Every capability is placed in one of three tiers, in this order of preference:

| Tier | Means | Acceptable when |
|------|-------|-----------------|
| 1 | Direct register access from the ARM | Always preferred |
| 2 | A documented firmware service over the mailbox property channel | The mailbox is the documented interface for that fact |
| 3 | An opaque firmware library or blob (VCHIQ, OpenMAX, dispmanx) | Tiers 1 and 2 are demonstrably closed |

The conclusion of the research, stated up front: **composition, the cursor and
scan-out are all tier 1.** The only tier 2 use is the mailbox property channel,
and only to learn facts and to obtain a framebuffer from firmware that has
already run. **No capability in this design requires tier 3**, so no blob is
loaded and the tier-3 question does not arise for graphics. Section 7 explains
why the QPU, which looks like the obvious tier-3 candidate, is in fact tier 1
and still not worth using.

---

## 2. Address spaces

Three address spaces matter and they are not the same numbers.

**ARM physical.** RAM starts at 0. Peripherals occupy a 16 MiB aperture. On the
256 MiB part the datasheet puts it at 0x20000000–0x20FFFFFF (§1.2.3). On the
1 GiB parts modelled here the same aperture sits at 0x3F000000: the peripheral
bus base is 0x7E000000 on every part, and it is translated to the ARM aperture
by the size of RAM, so 0x7E000000 − 0x3F000000 = 0x3F000000 on a 1 GiB part.

One detail of the platform record matters here, because it looks like a
contradiction and is not. The mailbox is declared twice, at two granularities:

* `mmio_devices` declares a claimable range at 0x3F00B000, 4 KiB, under the
  identity `bcm2835-mbox`. The MMIO inventory is page-granular by construction —
  its entries must be page aligned and page sized — so this is the page a driver
  claims, not the register block.
* `hardware.devices` declares the register window at 0x3F00B880, 0x40 bytes,
  under the model `bcm2835-mailbox`. This is what becomes the device-tree node.

They describe one device and they must agree: the window has to sit inside the
page that claims it. The platform record's build checks that agreement — a
record whose window falls outside every declared range, or a declared range that
holds two windows, is refused — so the two entries cannot drift apart. The
reason for the split is that the inventory cannot express a sub-page window, and
the register block starts 0x880 bytes into the page (which also holds the ARM
interrupt-pending registers at +0x200). A driver therefore matches and claims by
the inventory entry, then takes its exact register window from the device tree
rather than adding a hardcoded offset.

**Peripheral bus (VideoCore side).** The datasheet states the rule in §1.2.4: a
peripheral at bus address 0x7Ennnnnn is at ARM physical 0x20nnnnnn on the
256 MiB part. The same 0x7E000000 offset applies on the 1 GiB part. Every
address in the device tree is a bus address, so the mailbox is at bus
0x7E00B880 / ARM 0x3F00B880, the HVS at bus 0x7E400000 / ARM 0x3F400000, and the
V3D at bus 0x7EC00000 / ARM 0x3FEC0000.

**RAM bus aliases.** §1.2.3 states that RAM's bus addresses map onto the
uncached bus range starting at 0xC0000000. The 0x0-based alias and the ARM
physical address coincide, which is why a pointer word in a display-list
element holds an ARM physical address (section 5.4). A mailbox framebuffer
reply, by contrast, carries a bus address, which is why it must be reduced to
an ARM physical address before use. Masking with 0x3FFFFFFF produces the
physical address under either convention on a 1 GiB part; QEMU returns the
0x0-based alias directly (measured, section 10), while the datasheet's alias
rule implies firmware replies are in the 0xC0000000 range (inference from
§1.2.3, not measured here).

---

## 3. What the ARM is documented to reach, and what it can actually reach

§1.1 of the datasheet enumerates the peripherals "which may safely be accessed
by the ARM": timers, interrupt controller, GPIO, USB, PCM/I2S, DMA, I2C, the
I2C/SPI slave, SPI0/1/2, PWM and UART0/1. It then states:

> There are a number of peripherals which are intended to be controlled by the
> GPU. These are omitted from this datasheet. Accessing these peripherals from
> the ARM is not recommended.

The mailbox, the HVS, the pixel valves, the V3D and the HDMI block are all in
that omitted set. The datasheet has no mailbox chapter, no display chapter and
no VideoCore chapter at all — its table of contents ends at chapter 15 (USB).

**Conflict 1.** The primary datasheet says these blocks are GPU-controlled and
not recommended for ARM access. Reality: the ARM accesses all of them. The
mainline `vc4` driver programs the HVS, the pixel valves and the V3D from the
ARM on every BCM2835/2836/2837 boot; the mailbox property channel is the
documented ARM↔firmware protocol and is used by every bare-metal loader on this
part. The datasheet's statement is a support position, not a hardware
restriction, and the working behaviour is the one this design relies on. The
residual risk is real and is the subject of section 13.

The practical consequence for research is that the datasheet cannot answer the
questions this design asks about the display path. The HVS and pixel-valve
register interfaces are documented publicly only by the `vc4` driver (and,
partially, by the firmware's own behaviour). That is stated per claim below
rather than smoothed over.

---

## 4. The mailbox property channel (tier 2)

### 4.1 Registers

Base ARM physical 0x3F00B880, 0x40 bytes — the register window the record
declares as a hardware device, inside the 4 KiB page at 0x3F00B000 that the
MMIO inventory claims (section 2). The block is not in the datasheet (§1.1);
the layout below is the one the firmware's property protocol and every working
implementation use, and it was confirmed by reading the status register on the
QEMU model (section 10).

| Offset | Register | Notes |
|--------|----------|-------|
| 0x00 | read | pop one 32-bit word from the ARM→VC response FIFO |
| 0x10 | peek | read without popping |
| 0x14 | sender | sender ID of the word read |
| 0x18 | status | bit 31 full (write FIFO has no room), bit 30 empty (no response waiting) |
| 0x1C | config | FIFO configuration |
| 0x20 | write | push one word |

A word written to the write register carries the message's 16-byte-aligned bus
address in bits 31:4 and the channel number in bits 3:0.

### 4.2 Channels

Channel 8 is the property channel: ARM request, firmware response. Channel 1 is
the legacy framebuffer channel. Channel 9 exists for firmware-initiated requests
and no messages are currently defined on it. This design uses channel 8 only.

### 4.3 Message and tag format

A property message is a 32-bit-aligned buffer in RAM whose physical address is
handed to the mailbox:

* word 0: total buffer size in bytes, including the header, the end tag and any
  padding;
* word 1: request/response code — 0x00000000 in a request, 0x80000000 on a
  successful response;
* a sequence of tags;
* a terminating tag with identifier 0.

Each tag is: identifier, value-buffer size in bytes, a request/response code,
the value buffer, and padding to a 32-bit boundary. In a request the code's bit
31 is clear and bits 30:0 are reserved; in a response bit 31 is set and bits
30:0 give the number of bytes actually returned. A caller must therefore read
both the code and the returned length rather than assume the request's shape
was honoured.

### 4.4 Tags this design uses

| Tag | Identifier | Use |
|-----|-----------|-----|
| get firmware revision | 0x00000001 | record what firmware is running |
| get board model / revision / serial | 0x00010001 / 0x00010002 / 0x00010004 | identify the part for the platform record |
| get ARM memory | 0x00010005 | base and size of the ARM's RAM region |
| get VC memory | 0x00010006 | base and size reserved to the VPU |
| get/set clock rate | 0x00030002 / 0x00038002 | read the core clock the HVS is running at |
| set power state | 0x00028001 | documented, not used by this design |
| set physical width/height | 0x00048003 | display mode |
| set virtual width/height | 0x00048004 | buffer geometry (may differ from the visible mode) |
| set depth | 0x00048005 | bits per pixel |
| set pixel order | 0x00048006 | 0 = RGB, 1 = BGR |
| allocate buffer | 0x00040001 | request 4-byte alignment; reply is base (bus address) and size |
| release buffer | 0x00048001 | |
| get pitch | 0x00040008 | bytes per line — must be read, not assumed |
| set virtual offset | 0x00048009 | scroll within the virtual buffer |
| set overscan | 0x0004800a | inset the visible region |
| set palette | 0x0004800b | only meaningful for indexed depths |

Semantics worth recording because they are easy to get wrong:

* every setter's reply may differ from the request, and the documented
  behaviour is that an unsupported value returns the previous value (or 0)
  rather than failing, so the reply must be checked;
* allocate-buffer's request value is an *alignment*, not a size; the reply is
  the base address and size actually allocated;
* the base address in the reply is a bus address (section 2);
* the pitch in bytes may exceed width × bytes-per-pixel, so a stride taken from
  the reply is authoritative.

### 4.5 What the property channel cannot do

It configures one linear framebuffer and one mode. It has no notion of planes,
z-order, alpha, a cursor, a blit or a pattern. Every compositing and drawing
question in this document is outside it, which is why composition has to be
either software or direct HVS programming.

---

## 5. The display pipeline

The chain on this part is: memory → HVS (fetch, scale, compose) → pixel valve
(timing generation) → encoder (HDMI, DPI, DSI or composite). The ARM can reach
all three stages. This design only needs the HVS.

### 5.1 Pixel valve

Three pixel valves exist, at bus 0x7E206000, 0x7E207000 and 0x7E807000, each a
0x100-byte window (device tree `pixelvalve@7e206000`, `@7e207000`, `@7e807000`).
A pixel valve consumes one HVS output FIFO and generates the video timing for
its encoder; the HDMI block is at bus 0x7E902000, DPI at 0x7E208000.

The HVS must be told the active width and height of the mode (section 5.2), and
those values have to agree with the timing the pixel valve is generating. This
design does **not** program a pixel valve in its first phase: the firmware has
already established a mode before the ARM starts, and changing mode is a
separate problem from accelerating QuickDraw. Mode setting is called out as
explicitly out of scope in section 12 rather than silently assumed away.

### 5.2 HVS channels and the display list

The HVS is one block at bus 0x7E400000, a 0x6000-byte window (device tree
`hvs@7e400000`, `reg = <0x7e400000 0x6000>`). It has **three output channels**,
one per pixel valve. Each channel walks its own display list; all three lists
live in one 16 KiB SRAM inside the HVS register window.

Registers, as offsets from 0x7E400000. The offsets and fields are recorded here
as facts; the HVS is not in the datasheet, so the source is the `vc4` driver,
which is the only public register-level description of this block.

| Offset | Name | Contents |
|--------|------|----------|
| 0x000 | global control | block enable (bit 31); per-channel and DMA interrupt enables |
| 0x004 | global status | per-channel end-of-frame, end-of-line, underflow and short-frame flags; DMA response code (bits 15:14) and DMA error (bit 7) |
| 0x008 | block ID | read-only identifier; its encoding is not documented |
| 0x00C | display/encoder mux control | |
| 0x010 | profile | |
| 0x014 | dither | |
| 0x018 | end-of-line | |
| 0x020/0x024/0x028 | list pointer 0/1/2 | the channel's list, as a **dword offset** into the display-list SRAM |
| 0x02C | list status | |
| 0x030/0x034/0x038 | active list 0/1/2 | the list the channel is currently walking |
| 0x040/0x050/0x060 | channel control 0/1/2 | enable (31), reset (30), one-shot (29), active width (23:12), active height (11:0) |
| 0x044/0x054/0x064 | channel background 0/1/2 | auto-hsync (31), interlace (30), gamma (29), background fill enable (24) |
| 0x048/0x058/0x068 | channel status 0/1/2 | mode (31:30: disabled/init/run/end-of-frame), FIFO full (29) and empty (28), current line (11:0) |
| 0x04C/0x05C/0x06C | composite buffer allocation 0/1/2 | this channel's slice of the on-chip composite output buffer, as top (31:16) and base (15:0) pixel indices, 4-pixel aligned |
| 0x070 | alpha 2 | channel 2 only |
| 0x078 / 0x0E0 | gamma address / data | per-channel gamma LUT |
| 0x2000–0x5FFF | display-list SRAM | 16 KiB, shared by all three channels |

Three facts follow directly and shape the whole design:

1. **A plane is not a register.** There is no per-plane register file and no
   fixed number of planes. A plane exists only as an element inside a channel's
   display list.
2. **The list is in HVS SRAM, not in system memory.** Programming a plane means
   writing words into the HVS window at 0x2000 and above, then pointing the
   channel at that offset.
3. **The first 128 bytes of that SRAM belong to the firmware.** The `vc4`
   driver records that it never overwrites the setup from the bootloader —
   the first 32 dwords — precisely so that the screen is not scrambled during
   the transition from firmware display to runtime display. This design keeps
   the same reservation, for the same reason (and it is what makes recovery
   possible; section 13).

### 5.3 Display-list element layout

A channel's list is a sequence of elements followed by a terminator. Each
element begins with a control word that includes the element's length in
dwords, so the executor can step from one element to the next; the list ends at
a word with the end bit set. The words after the control word are the plane's
geometry, source and blend state. For a single-plane RGB element with no
scaling the element is seven dwords (28 bytes); scaling and multi-plane YUV add
words.

Word order, with fields as bit ranges:

| Index | Name | Fields |
|-------|------|--------|
| 0 | control | 31 end-of-list, 30 valid, 29:24 element length in dwords, 21:20 tiling, 19 alpha-mask enable, 18:17 colour-key mode, 16 horizontal flip, 15 vertical flip, 14:13 component order, 12:11 low-bit expansion, 10:8 scaling mode for channel 1, 7:5 scaling mode for channel 0, 4 unity (no scaling), 3:0 pixel format |
| 1 | position 0 | 31:24 fixed alpha, 23:12 destination y, 11:0 destination x |
| 2 | position 1 — **omitted when unity**, every later word then shifts down one | 27:16 scaled height, 11:0 scaled width |
| next | position 2 | 31:30 alpha mode, 29 premultiplied, 28 alpha mix, 27:16 source height in pixels, 11:0 source width in pixels |
| next | position 3 | context word, written by the HVS; the value the ARM places here is not used |
| next | pointer 0 | source address for channel 0 |
| next | pointer context | written by the HVS |
| next | pitch 0 | 15:0 source pitch in bytes |
| next | YUV only | further pointers, pitches and colour-space conversion words |

Details that matter operationally:

* The element length in the control word must equal the number of dwords the
  element actually occupies, including the control word. The executor trusts it.
* Pointer words accept any byte address; there is no alignment requirement in
  the interface, though alignment is obviously desirable for fetch efficiency.
* Pointers are 0x0-based bus addresses, which on a 1 GiB part equal the ARM
  physical address (section 2).
* Destination x and y are 12-bit fields, so the maximum expressible coordinate
  is 4095 and the fields are not signed. A window dragged past the left or top
  edge must be clipped by adjusting the source pointer and source size, not by
  a negative destination. This is a real constraint on a plane-based window
  manager and is one of the reasons the driver owns clipping rather than the
  application.
* Source width and height are 12-bit fields (4095 pixels).
* Pitch is a 16-bit field: at 32 bits per pixel the largest expressible stride
  is 16383 pixels. A 1920-pixel-wide surface needs 7680 bytes and fits.

### 5.4 Pixel formats and byte order

Two independent fields select the memory layout: the pixel format (control word
bits 3:0) and the component order (bits 14:13). The format names the component
widths; the order names how the components are arranged in memory.

Formats relevant here:

| Value | Format | Use |
|-------|--------|-----|
| 0 | RGB332 | 8-bit packed |
| 1 | RGBA4444 | 16-bit with 4-bit alpha |
| 2 | RGB555 | 16-bit, no alpha |
| 3 | RGBA5551 | 16-bit with 1-bit alpha |
| 4 | RGB565 | 16-bit, no alpha |
| 5 | RGB888 | 24-bit packed |
| 6 | RGBA6666 | 24-bit with 6-bit alpha |
| 7 | RGBA8888 | 32-bit with 8-bit alpha |
| 8–11 | YCbCr 4:2:0 / 4:2:2, 2- and 3-plane | video |
| 12 | H264 | video |
| 13 | palette | indexed; see conflict 2 |
| 14/15 | YUV444/RGB, AYUV444/RGB | video |
| 16 | RGBA1010102 | 10-bit, later generations |
| 17 | 10-bit YCbCr | later generations |

The component-order field is two bits with two naming conventions in the
register documentation: for the 32-bit and 24-bit layouts the values 0–3 are
RGBA, BGRA, ARGB, ABGR; for the 16-bit layouts they are XBRG, XRBG, XRGB, XBGR.
Both are the same field and the same four values.

The one mapping this design depends on: a 32-bit surface whose memory bytes are
B, G, R, X in increasing address order — which is what System already draws, a
0x00RRGGBB little-endian pixel — is format RGBA8888 with component order ABGR.
The existing framebuffer therefore needs no conversion and no format change to
become a plane. A 16-bit RGB565 surface is format RGB565 with order XRGB.

**Conflict 2.** The format enumeration defines a palette (indexed) format, and
the mailbox property channel defines a palette tag, but the `vc4` driver
supports no indexed format at all: its format table contains no indexed entry.
Whether the palette path works on this part is therefore not established by any
source, and the design does not use it. This is not a limitation in practice,
because the resident compositor already renders into a 32-bit surface.

### 5.5 Alpha and blending

Three mechanisms exist, and they compose:

* **Per-pixel alpha** from the source format, selected by alpha mode 0
  (pipeline).
* **Plane-global alpha** in position word 0 bits 31:24, selected by alpha mode
  1 (fixed), 2 (fixed, non-zero) or 3 (fixed, over 0x07). Bit 28 of position
  word 2 mixes the plane alpha with the per-pixel value instead of replacing
  it.
* **Premultiplication**, signalled by bit 29 of position word 2, for sources
  stored premultiplied.

Blending is over whatever is beneath in the same channel: the previous element
in the list, or the background fill if there is none (section 5.7). This is
what makes overlapping windows free: an obscured window's pixels are never
fetched where an opaque plane covers them.

For an opaque 32-bit desktop surface, the correct configuration is format
RGBA8888 with order ABGR and per-pixel alpha off (or alpha mode fixed at
opaque), so no blending work happens at all. For an antialiased or
alpha-blended window, the same format with per-pixel alpha on.

### 5.6 Colour key and alpha mask

The control word reserves a two-bit colour-key mode: 0 disabled, 1 luma or
common RGB key, 2 match (turn matching pixels transparent), 3 replace with the
value in a key mask word. Alpha-mask enable is bit 19.

Both features are defined in the register set and **neither is used by the
`vc4` driver**, which never sets a key mode or the alpha-mask bit. The
consequence is that the register fields are documented but the supporting data
— where the key value and the mask live within an element — is not established
by any source available to this design. The design does not depend on either:
transparency comes from per-pixel alpha, which is exercised on every frame by
every implementation. This is recorded as a residual uncertainty in section 15
rather than designed around.

### 5.7 Background

The channel background register on this generation exposes an enable for
background fill (bit 24) plus interlace and gamma flags — **no colour field**.
The `vc4` driver enables fill unconditionally on generations before the sixth
and never programs a fill colour. So on this part the fill is a fixed colour
that the register interface does not let software choose.

What that means in practice: the fill covers the area no plane reaches, and it
is what shows through an alpha-blended plane, but it cannot carry the desktop
pattern. A Mac-style patterned desktop must therefore be a plane. That is not a
cost worth avoiding — a full-screen plane at unity scale needs no line-buffer
memory and no scaling words — but it does mean the desktop pattern is fetched
every scanline rather than being free.

### 5.8 The transposer

There is one block that can write composed pixels back to memory: the
transposer at bus 0x7E004000, a 0x20-byte window with GPU interrupt 11 (device
tree `txp@7e004000`). It has destination pointer and pitch registers and takes
the output of an HVS channel, which is put into one-shot mode while it is being
fed to the transposer. It is a writeback path for the composed frame, not a
drawing engine: it composes planes and writes the result, it cannot fill, blit
or read a destination.

It is worth recording because it is the only mechanism on this part for
producing a composite off-screen, and because it is the natural implementation
of "take a snapshot of a plane stack into a buffer" if the compositor ever needs
one. It does not accelerate any QuickDraw primitive, and it costs a full-frame
write, so it is not part of the first phases.

### 5.9 Installing a list, and knowing it took effect

Two registers per channel: the list pointer (0x020/0x024/0x028) is where the
channel should look next, and the active list (0x030/0x034/0x038) is what it is
actually walking. Writing the list pointer does not take effect mid-frame: the
channel latches a new list at a frame boundary, and the active-list register
changes when it does. That register is the acknowledgement that a flip
completed, and it is how the `vc4` driver decides a page flip has landed.

This gives the design a retirement rule that needs no interrupt: publish a new
list, and only reuse or overwrite the previous list's storage once the active
list register no longer names it. Because the whole display list for a scene is
at most a few hundred bytes, double-buffering two lists is cheap and removes
the question of tearing during an in-place edit.

### 5.10 Interrupts

The HVS raises GPU interrupt 33 (device tree `interrupts = <2 1>`, i.e. GPU IRQ
32 + 1). The ARM sees GPU interrupts through the ARM control block at
0x7E00B000: §7.5 of the datasheet lists a basic pending register at 0x200, IRQ
pending 1 at 0x204 covering GPU interrupts 0–31, and IRQ pending 2 at 0x208
covering 32–63 (§7.2 explains the two GPU pending registers). GPU interrupt 33
is therefore bit 1 of the register at 0x7E00B208, once the corresponding enable
is set.

**This design does not take that interrupt.** Everything composition needs is
pollable: the active-list register for flip completion, and the global status
register's per-channel end-of-frame and underflow flags for health. Avoiding the
ARM-side GPU interrupt routing removes a whole class of bring-up risk from the
first phase; it can be added later if polling at frame rate ever shows up in a
profile.

### 5.11 Capacity

The display-list SRAM is 16 KiB, shared by all three channels, with the first
128 bytes reserved to the firmware. At 28 bytes per unscaled RGB element that
is a structural ceiling of roughly 580 elements in total, not per channel. That
ceiling is not the practical limit.

The practical limit is the composition bandwidth budget, and the hardware
exposes the two quantities that make it up:

* **Composite output buffer.** The HVS composes through an on-chip buffer of
  20736 pixels on this generation, partitioned between the three channels by
  the per-channel top/base allocation registers (section 5.2). This is what
  bounds how many lines of composition can be in flight.
* **Aggregate plane load.** Each plane's cost is its source size times its
  bytes per pixel times the refresh rate, adjusted for scaling; the `vc4`
  driver sums this over the active planes and derives the core clock the HVS
  would need, and refuses a configuration that does not fit the achievable
  clock rather than allowing silent underflow. The sum is the real plane
  budget.

Two constraints reduce a plane's cost to its minimum:

* **Unity scaling.** When the destination size equals the source size the
  element is marked unity, no scaling words are emitted, and no line-buffer
  memory is allocated. Line-buffer memory is only required when vertical
  scaling is used. A window plane at 1:1 therefore costs only fetch bandwidth.
* **Opaque planes.** An opaque plane covering the screen lets the HVS skip
  blending for everything below it.

The design's working assumption is a scene of a handful of full-size planes
(desktop, menu bar, a few windows, cursor) and a measured ceiling established
on hardware, not a theoretical maximum. The `vc4` driver's own limit of sixteen
overlays plus a primary and a cursor is a driver policy, not a hardware
constant, and this design should set its own after measuring.

---

## 6. The cursor

### 6.1 What the hardware provides

There is **no dedicated cursor hardware** on this generation; the cursor is an
ordinary plane. The HVS register set is a list of plane elements per channel
plus per-channel control, background and gamma — there is no cursor register,
no cursor size field, no hotspot register and no cursor-specific alpha mode.
The `vc4` driver's cursor is a plane created through the same plane
initialisation as any other, with the same format list, and created last so that
it sits on top of the z-order; the driver's own comment on plane creation states
that planes are not limited by a fixed number of physical registers but by
display-list space and the 28-byte minimum element size.

This answers the questions that a dedicated cursor overlay would otherwise
raise:

| Question | Answer on this part |
|----------|---------------------|
| Dimensions | No hardware limit; any source size that fits the 12-bit width/height fields. Not restricted to 32×32 or 64×64, and not required to be a power of two. |
| Formats | Any plane format. RGBA8888 gives 8-bit per-pixel alpha; RGBA5551 gives 1-bit alpha. |
| Transparency | Per-pixel alpha (8-bit, optionally premultiplied) or 1-bit alpha. Colour key is defined but unexercised (section 5.6). |
| Positioning | Whole pixels only, 12-bit unsigned x and y. No sub-pixel positioning. |
| Hotspot | None in hardware. The hotspot is software bookkeeping: the plane's destination is the pointer position minus the hotspot. |
| Independence | None. The cursor is an element in the same display list and therefore in the same z-order stack; it must be the last element to be on top. |

### 6.2 The three-part QuickDraw cursor and the conversion decision

A QuickDraw cursor is a bitmap, a mask and a hotspot. The plane wants an RGBA
bitmap and a destination position. The driver must therefore hold a converted
form.

**Decision: convert on change, not on move.** The reasoning is a rate argument,
not a preference. A cursor changes shape rarely — a few dozen distinct cursors
across the whole application set, and a change per application-defined event at
most — while it moves on every mouse event, which is the highest-frequency
input in the system. Paying 256 pixels of conversion work (a 16×16 cursor) when
the shape changes and nothing when the pointer moves puts the cost on the rare
event and leaves the frequent event free. Maintaining both representations
would put a synchronisation obligation on every change for no benefit, because
the move path never reads the QuickDraw form.

The cost is bounded and one-off per change: 16×16 pixels expanded from
bitmap+mask into ARGB, written once into a small buffer that the cursor plane
points at. That buffer's address never changes while the cursor shape is
unchanged, so a move does not touch the cursor's pixels at all.

**Limitation, stated plainly.** QuickDraw's cursor can invert what is beneath it
(source XOR with the mask selecting transparent, invert or black). A plane
blends; it cannot read back the content beneath it, so an inverting cursor
cannot be expressed as a plane. The resident cursor service already handles
mask compositing, source XOR and obscuring (see `docs/CURSORS.md`); cursors that
require inversion must either be approximated as opaque-over or composited by
the CPU into the ARM-drawn plane, which costs a save-under/restore exactly as
today. The plane path is a large win for ordinary mask-blended cursors and is
not a complete replacement for the XOR case.

### 6.3 Cost of a move

With a double-buffered display list (section 5.9), a pointer move is: rewrite
one 32-bit position word in the inactive list, then publish that list by
writing the channel's list pointer. The hardware switches at the next frame
boundary. The framebuffer is not touched, nothing is saved under, nothing is
restored, and no window repaints because the cursor crossed it.

### 6.4 Making "the cursor is responsive" measurable

The system already emits observable evidence that can be asserted on rather
than judged by feel:

* input records in the trace (`trace::INPUT`, `trace::PRESENT`,
  `trace::FRAME_COPY`, `trace::WINDOW`) from `system/src/trace.rs`;
* application-level markers such as `FINDER_CLICK_HANDLED` and
  `FINDER_DRAG_ENDED moved|copied|rejected|cancelled` (`apps/finder/src/main.rs`,
  `apps/finder/src/dragging.rs`), which the existing Finder gates already count
  in serial output (`crates/mac-toolbox/tests/finder_drag.rs`).

The acceptance artefact for the cursor plane is therefore a trace in which a
pointer move produces an input record and a plane-origin update, and **no**
`FRAME_COPY` record and no window-outline XOR record. That is a checkable
statement about work not done, which is exactly what the performance bar in the
project rules asks for. On a machine with no HVS the same trace must show the
old behaviour, so the difference between the two is the measurement.

---

## 7. The V3D and the QPU

### 7.1 What the block is

The V3D is the 3D pipeline: a binner, primitive setup, and a shader core made of
quad processors (QPUs). Its registers are at bus 0x7EC00000 on this part
(*VideoCore IV 3D Architecture Reference Guide*, Appendix B, Table 95, which
lists 0x7ec00000 for BCM2835), 0x1000 bytes, GPU interrupt 10. The pipeline is
driven by control lists in memory which the control list executor walks, feeding
primitives and state changes to the hardware; the guide's section 9 defines the
control record formats, and section 3 defines the QPU instruction set.

### 7.2 Reachability: this is tier 1, not tier 3

The V3D's clock comes from the clock manager block at bus 0x7E101000 and its
reset from the power-management block at bus 0x7E100000. Both are ARM-accessible
register blocks on this part, and the device tree says so explicitly: the V3D
node declares its clock as the clock manager's V3D clock and its reset as the
power-management block's V3D reset (`bcm2835-common.dtsi`, `v3d@7ec00000` with
`resets = <&pm BCM2835_RESET_V3D>`; the `pm` node is the power-management block
with its own V3D clock reference). The `vc4` driver enables both from the ARM
and programs the V3D directly, with no mailbox involvement in block control.

So the QPU is **not** sequestered on BCM2837. It is reachable by direct register
access, which places it in tier 1, and the widely repeated claim that 3D on this
part requires firmware services is not supported by the sources examined here —
the driver that runs on this silicon does it without them. The one firmware
interaction in the display path is a *clock-rate query* over the mailbox, used
to compute the HVS's maximum core clock, which is tier 2 and optional.

### 7.3 Why the design does not use it anyway

There is no 2D blit engine anywhere on this part, and the support for that is an
enumeration rather than an absence of documentation. The datasheet's list of
ARM-accessible peripherals (§1.1) contains no graphics block at all, so every
candidate is a GPU block. Of those, the 3D guide describes exactly one compute
pipeline — control lists feeding a primitive tile binner, primitive setup, and
QPU shaders (§9 and §3) — and no copy, fill or raster-blit stage in it. The HVS
fetches planes and blends them at scan-out, with no destination in memory
(section 5.2). The transposer writes a composed frame and does nothing else
(section 5.8). There is no fourth block for a blitter to be, and `vc4`, which
drives every one of these blocks, programs no blit path.

A rectangle fill or a source-over blit expressed on the V3D is therefore a
rasterised quad with a shader program, going through the binner, with per-tile
setup and a command list to build. That is a large body of work — a
control-list builder, binner memory management, and shader programs — with a
failure mode that is worse than the HVS's (section 13), and its benefit over the
HVS for QuickDraw's actual operations is not established.

The decision is a cost/benefit one and is recorded as such: the HVS removes the
dominant desktop costs (scene composition, window movement, cursor) with a
28-byte element format and no shader compiler; the V3D would add a rasteriser in
front of the same memory bandwidth for fills and blits that a 1.2 GHz A53 does
in cache-friendly loops. The V3D stays available as a later option if a specific
primitive is measured to be the bottleneck, and section 14 does not schedule it.

---

## 8. VCHIQ, OpenMAX and dispmanx (tier 3, not used)

VCHIQ is the ARM↔VPU message transport; OpenMAX IL and MMAL are firmware
services reached through it; dispmanx is the firmware's own compositor, which
takes layers over VCHIQ and programs the HVS on the VPU side. All three are
opaque: their implementation lives in the VPU firmware, no register interface is
published, and the mailbox exposes only an initialisation tag.

**Finding: none of them is needed.** Composition is directly programmable from
the ARM (section 5), so dispmanx is redundant. The QPU is directly programmable
(section 7), so OpenMAX is not required for acceleration. That leaves VCHIQ with
no consumer in this design.

Because tier 3 is never entered, the blob question is answered without loading
one: the design uses direct registers (tier 1) and, for the framebuffer
handoff and clock queries, the mailbox property protocol (tier 2), which is a
documented message format rather than an opaque library. The tier-1 and tier-2
paths were not merely preferred; they were shown to exist and to be what working
software on this part uses.

---

## 9. Tier and verification summary

| Capability | Tier | Evidence | Verifiable on `raspi3b`? |
|------------|------|----------|--------------------------|
| Linear framebuffer + mode | 2 (mailbox) | property channel tags; measured reply | **Yes** |
| Scan-out of one plane | 1 (HVS) | HVS list pointer + element; `vc4` | No — HVS not modelled |
| Multi-plane composition | 1 (HVS) | element list, z-order, alpha | No |
| Cursor as topmost plane | 1 (HVS) | no cursor hardware; ordinary plane | No |
| Background fill | 1 (HVS) | background register bit 24 | No |
| Window drag by plane move | 1 (HVS) | position word + list publish | No |
| Off-screen plane composition | 1 (transposer) | destination pointer/pitch registers; one-shot HVS channel | No — placeholder only |
| Rect fill, line, text, pattern | — | no 2D engine exists | n/a (CPU) |
| Blits and ROP modes | — | no ROP hardware exists | n/a (CPU) |
| QPU-drawn primitives | 1 (V3D) | 3D guide; clock/reset in DT | No — placeholder only |
| dispmanx / OpenMAX / VCHIQ | 3 | firmware-only | No |

The single most important line in that table is the third column of the second
row: **the HVS is not modelled by QEMU**, so composition cannot be tested in
this environment at all. Section 10 gives the measurements behind that
statement, and section 14 orders the work so that the part which *can* be
tested now is built first.

---

## 10. What QEMU's `raspi3b` actually models (measured)

Measured with QEMU 11.1.0 (`qemu-system-aarch64 -machine raspi3b`), using a
scratch AArch64 probe that talks to the PL011 directly. These are observations
about the model, not about silicon.

**Mailbox.** The property channel works and behaves as documented. The status
register read 0x40000000 at reset — bit 30 set, meaning the response FIFO was
empty, which is the bit's meaning in the datasheet-independent implementation.
A request containing set-physical-width/height 800×600, set-virtual 800×600,
set-depth 32, set-pixel-order 1, allocate-buffer and get-pitch returned
request code 0x80000000 (success), an allocate-buffer reply of base
0x3C100000 with 0x001D4C00 bytes (1 920 000 = 800×600×4) and value length 8,
and a pitch of 0x0C80 (3200) with value length 4. Two things follow: the reply's
base is the 0x0-based alias (section 2), and the pitch must be read because the
model returns a real value rather than an echo.

**HVS.** Not modelled. QEMU's peripheral container covers
0x3F000000–0x3FFFFFFF, but only leaf regions are inserted into the address
space, so an address inside the container with no device behind it falls through
to RAM. Measured consequence: reading the HVS control, channel control, channel
status and list-pointer registers returned zero; reading the display-list SRAM
returned zero; and **writing a marker into the display-list SRAM and reading it
back returned the marker**. The HVS window in this model is ordinary memory.

This is the most dangerous fact in the document. An HVS driver run on this
model would not fault, would not fail a probe, and would not display anything:
every write would succeed and every read would return what was written. A
smoke test that checks "the register write stuck" passes on a machine with no
HVS. Identification must therefore come from the platform record and the SoC
identity, never from a register read-back.

QEMU's source corroborates this: its BCM283x platform header defines an HVS
offset, but no HVS device is instantiated in the peripheral model, so the offset
is unused.

**Pixel valve.** Not modelled; the pixel-valve window likewise falls through to
RAM.

**Transposer.** Not modelled. QEMU instantiates an unimplemented placeholder at
0x3F004000, so its registers read as zero and writes are discarded.

**V3D.** A placeholder only. QEMU instantiates an unimplemented device for the
V3D, but at 0x3FC00000 rather than the silicon's 0x3FEC0000, and reads return
zero. It is not a model of the block in any useful sense, and its address does
not match the datasheet's.

**Display output.** The model does implement a single linear framebuffer
(`bcm2835-fb`) driven by the property channel: geometry, depth, pixel order,
pitch, virtual offset, palette and blank. It scans that one buffer out and
performs no composition, has no planes, no alpha blending and no cursor.

**Conclusion.** On this environment the testable surface is exactly: the
mailbox property protocol, framebuffer allocation and geometry, writing pixels
into that one buffer, and everything above the driver boundary — module
selection, probing, capability reporting, the plane *model* as data, damage
tracking and the cursor conversion as data. Everything that touches HVS, pixel
valve or V3D registers is hardware-only.

---

## 11. QuickDraw verdicts

The question for each primitive is not only "would hardware be faster" but "is
there a hardware path at all". On this part there is no drawing engine: the HVS
fetches and blends, the only write-back path is the transposer (which composes
planes into a buffer rather than drawing into one), and the V3D is a 3D
rasteriser (section 7.3). So every drawing primitive stays on the CPU, and the
hardware's contribution is to remove work *around* the drawing.

| QuickDraw operation | Hardware path | Verdict |
|---------------------|---------------|---------|
| Rectangle fill | None. No fill engine; the background fill covers only where no plane reaches and its colour is not programmable (section 5.7). | CPU. A large static fill (desktop) is better expressed as a plane. |
| Line drawing | None. | CPU. |
| Text (glyph blits) | None. Text is clipping plus blits, both CPU. | CPU. A text *run* that never changes could be a plane, but that is a caching decision, not acceleration. |
| CopyBits, `srcCopy` | No blit engine. The HVS reads planes and blends them at scan-out; it has no destination in memory. The transposer writes a composed frame, it does not copy a rectangle. | CPU. |
| CopyBits with mask / `srcOr` / `srcXor` / `srcBic` | No ROP hardware. Blending exists only at scan-out, over the composed result, not against a destination in memory. | CPU. |
| Pattern fill | No pattern repetition. The tiling field selects the *memory layout* of a source (linear, 64-byte, 128-byte, 256-byte/T), not UV repetition. | CPU. A static patterned desktop is a plane carrying the pattern. |
| Region clipping | Not a drawing operation, but: the hardware clips each plane to the screen and to the z-order, so obscured content is never fetched and a window never has to repaint what is behind it. Destination coordinates are 12-bit unsigned, so off-screen edges are clipped by the driver adjusting source pointer and size. | Hardware for composition; CPU for drawing inside a surface. |
| CopyBits into the visible framebuffer (present) | The current path composites the scene into the front buffer every frame. With planes there is no front buffer to composite into: each surface is scanned out directly. | **Hardware: the copy disappears.** This is the single largest saving. |
| Window drag | Plane position update; both old and new positions are correct without repainting either. | **Hardware.** |
| Overlapping windows | Z-order in the display list; the occluded window's pixels are not fetched or redrawn. | **Hardware.** |
| Cursor move | Cursor plane position update (section 6.3). | **Hardware.** |
| Menu bar, desktop pattern | Static planes, re-composited by the hardware every frame at no CPU cost. | **Hardware.** |
| Compose a plane stack into a buffer | The transposer takes an HVS channel's output and writes it to memory (section 5.8). | Available, but it composes rather than draws, and costs a full-frame write. Not used in the first phases. |
| Scroll of a window's content | The window's surface is scrolled in its own buffer by the CPU, then the plane is unchanged. Optionally the virtual-offset mechanism of the property channel can scroll a single full-screen buffer, but not a plane. | CPU for content, hardware for display. |

The honest summary: **no QuickDraw drawing primitive is accelerated; the
compositing and presentation of the results are.** That is worth doing, because
the per-frame scene copy and the repaint of obscured content are the costs that
scale with window count and pointer motion, and both go to zero.

---

## 12. Where this fits in System RISC

### 12.1 Driver shape

The display path is an external driver crate under `drivers/`, following the
existing model: a `module.json` declaring the video device class in
`interfaces`, a device match on the mailbox window's bus key, `src/lib.rs` for
the safe engine and `main.rs` using `mac_driver::export_driver!`. The kernel
selects it by declared class, and a runtime probe claims the device; no code
path selects a driver by filename. `drivers/virtio-gpu` is the closest existing
example and already demonstrates the attach/present/prepare/commit/abort/release
lifecycle, the 250 ms resize debounce and the typed refusals.

Two classes of capability are involved and they should stay separate:

* **scan-out and presentation**, which the existing `VIDO` class already
  expresses: attach with a mode and limits, receive damage, present it;
* **composition**, which `VIDO` does not express: how many planes, which
  formats, whether alpha and a cursor plane are available, and a way to publish
  a scene.

The composition contract is a new class (or a new major version of the video
class), negotiated rather than assumed: a driver that can only scan out one
buffer reports no composition capability and is used exactly as today. The
contract itself is designed at implementation time from this description, not
specified here.

### 12.2 Coexistence with the existing framebuffer, and the fallback

The bootloader keeps its current job — early console and MMU bring-up — and the
firmware has already established a mode and a linear buffer before the ARM
starts. System takes that buffer over, as it does today.

**The firmware's buffer becomes plane 0.** It is not replaced and its address
does not change. Plane 0 is the "the ARM draws here" surface: the resident
compositor keeps drawing into it exactly as it draws into the current front
buffer. The other planes layer *on top of* it, and the HVS composites them.

This choice is what keeps the software path simple and the fallback honest:

* With no HVS, or with a driver that reports no composition capability, the
  system behaves exactly as it does today: one buffer, software composition,
  the existing damage tracking and the existing `FRAME_COPY` trace.
* With composition available, plane 0 still exists and still receives anything
  the compositor draws; windows that are not promoted to planes simply remain
  part of plane 0's content.
* The two paths are the same driver crate with a capability gate, not two
  drivers and not a duplicate stack.

The display list is the only thing that changes: the firmware's list stays
untouched at offset 0 of the SRAM, and the driver publishes a list of its own
from offset 32 dwords onward and points the channel at it. Leaving the original
in place is what makes recovery possible (section 13).

### 12.3 Damage and the present path

Today, presenting accumulates an exact damage rectangle, copies the scene into
the front buffer, and records `FRAME_COPY` with the rectangle's size. With
planes the present path splits into two kinds of work that must not be
conflated:

* **content damage** — pixels changed inside a surface. The owner writes them
  into its own buffer. No copy to a front buffer, no scene-level invalidation.
* **scene damage** — a plane moved, appeared, disappeared or changed z-order.
  This is a display-list update, bounded by the number of planes, not by pixels.

Keeping those separate is the same principle the project already applies to
cursor and control damage: a change is repainted only where the changed object
is. A cursor move must never produce a scene-level repaint, and a window drag
must never produce a pixel copy.

### 12.4 Concurrency and lifetime

The compositor is already serialised and owns the front/back buffers; the
display-list double buffer and the publish step belong to that same
serialisation, not to a new lock. Plane source buffers must stay mapped and
owned for as long as a published list names them, which means a plane's buffer
is retired only after the active-list register no longer names the list that
references it (section 5.9) — the same "preflighted lease stays pinned across
its use" discipline the resident rules already require for DMA.

The current scheduler admits one active hart; the HVS work does not change that
and must not be used as an argument to change it.

---

## 13. Risks and the debugging story

**A malformed display list does not fault the ARM.** The HVS reads the list from
its own SRAM and fetches from the addresses in it. A wrong element length makes
the executor step into the middle of an element; a wrong pointer makes it fetch
from memory the OS does not own; a missing end bit makes it walk past the end of
the list. None of these raises an exception on the ARM. The visible symptoms are
a black screen, a frozen scan-out, or corrupted pixels.

What is observable, and therefore what the debugging story rests on:

* **Fetch errors are reported.** The global status register carries a DMA
  response code and a DMA-error flag, and also the per-channel end-of-frame,
  end-of-line, underflow and short-frame flags. A bad pointer is therefore
  detectable after the fact rather than being completely silent. A bad *list
  structure* is not.
* **The active-list register tells the truth.** If the channel's active list
  never changes to the published list, the publish did not take, and the reason
  is usually that the channel is disabled or that the list pointer is outside
  the SRAM.
* **The firmware's list is still there.** Because the first 128 bytes are never
  overwritten, recovery is a single register write pointing the channel back at
  offset 0. That is the watchdog action: if the global status register's
  per-channel end-of-frame flag stops advancing for a bounded number of frames,
  or if the DMA-error flag is set, revert to the boot list and report a
  diagnostic. This is the only reason the reservation is worth its 128 bytes.

Mitigations that belong in the design rather than in debugging:

* build the list into ordinary memory first and validate it structurally before
  copying it into the HVS — element lengths consistent with the total, a
  terminator present, every pointer inside a buffer the OS owns, no element
  overlapping another;
* never publish a list that references memory whose ownership has been
  released;
* publish only through the double buffer, never by editing the list the channel
  may be walking;
* keep the scene small enough that the fetch budget is obviously sufficient
  until hardware measurements say otherwise.

**QEMU cannot catch any of this.** The model has no HVS, and its window is RAM,
so a structurally perfect but semantically wrong list will look fine in
emulation. The only defence is that the list builder is tested against a
description of the format, and the hardware bring-up is done in the order in
section 14 — one plane, then several, then the cursor.

**Taking the HVS from the firmware is a one-way change within a boot.** Once the
channel is pointed at our list, the firmware's compositor is no longer what is
on screen. If the OS later needs the firmware's display services (it does not,
by design), that would have to be undone explicitly.

---

## 14. Prototype order and measurable artefacts

The order is dictated by the verification split, not by ambition: the part that
can be tested in this environment comes first, and the part that needs silicon
is designed so that everything except the register writes is already proven.

**Phase 1 — mailbox scan-out on `qemu_arm64`.** A video driver that obtains the
framebuffer and mode over the property channel, reports geometry and pitch, and
presents damage by writing into that buffer. It replaces the ramfb path on this
platform: the resident display adapter is still written against a firmware
configuration device that this platform does not declare, so the mailbox path is
what makes scan-out exist here at all, and it is the foundation every later
phase sits on. Success is measurable and fully testable here: the desktop
reaches its ready marker with the display driver attached, the reported mode and
pitch match what the property channel returned, a deliberately unsupported mode
is refused with a typed cause rather than a black screen, and the existing
present/damage trace is unchanged in shape.

**Phase 2 — the plane model with a software backend.** Z-order, per-plane
clipping, per-plane content damage and the cursor conversion exist as a scene
model with a software compositor behind the same interface the HVS backend will
implement. This is where the semantics are nailed down and where they can be
tested without hardware: a scene of N planes composed by software must be
pixel-identical to the single-buffer composite it replaces, an off-screen window
must clip identically to the current path, and a cursor move must produce no
`FRAME_COPY` record. Nothing in this phase depends on the HVS existing.

**Phase 3 — HVS backend, hardware only.** Bring-up in the order that limits
damage: identify the enabled channel and its list from the registers rather
than assuming channel 0; publish a single plane pointing at the firmware's
framebuffer with the format and order that match its existing byte layout, and
verify the picture is unchanged; then add the background fill; then a second
plane and confirm z-order; then promote one window to its own plane; then the
cursor plane. Success criteria, all measurable on the target: the picture is
identical to the software composite for the same scene; `FRAME_COPY` records
disappear from the present path; a window drag produces no pixel copy and no
window-outline XOR, only a plane-position change; the active-list register
advances to the published list each frame; no underflow or DMA-error flag is
ever set during the run; and after a deliberate fault injection (a list with a
bad pointer) the watchdog reverts to the boot list and the diagnostic appears.

**Phase 4 — per-window planes and damage narrowing.** Windows that are
promoted to planes stop contributing to plane 0's damage; the menu bar and
desktop pattern become static planes; the software composite becomes the
fallback rather than the norm.

Not scheduled: V3D work (section 7.3), pixel-valve mode setting, and the ARM-side
HVS interrupt (section 5.10).

---

## 15. Residual uncertainties

Each of these is specific, and each names what would resolve it.

1. **Sign of the destination coordinate fields.** The HVS destination x and y
   are 12-bit fields and the `vc4` driver always writes non-negative, clipped
   values, so nothing establishes whether the hardware treats them as signed
   offsets. Resolution: a single experiment on hardware placing a plane at a
   large unsigned value with a known source. Until then the driver clips, which
   is required anyway.
2. **Placement of the colour-key and alpha-mask data.** The control word defines
   a key mode and an alpha-mask enable, and the key modes reference a key mask
   word, but no source shows where that word sits in an element because the
   driver never enables either feature. Resolution: not needed by this design;
   per-pixel alpha covers the cases.
3. **Which HVS channel the firmware's boot display uses.** The reserved boot
   list is at offset 0 of the SRAM, but which of the three channels walks it is
   not documented, and the design must not assume channel 0. Resolution: read
   the per-channel enable and active-list registers at probe time and follow the
   hardware. This is a designed-in discovery step, not an assumption.
4. **Sustainable plane count and bandwidth.** Display-list space bounds a scene
   at roughly 580 minimum-size elements; nothing public quantifies the fetch
   bandwidth at 800×600 with 32-bit surfaces. Resolution: measure on hardware,
   and keep the initial scene small.
5. **Whether the palette (indexed) plane format works.** Defined in the format
   enumeration and supported by a mailbox palette tag, but never used by the
   driver (conflict 2). Resolution: not needed; the compositor is 32-bit.
6. **QEMU versus silicon.** Every measurement in section 10 describes QEMU
   11.1.0's model. The framebuffer base address convention in particular differs
   between the model (0x0-based alias) and what the datasheet's alias rule
   implies for firmware replies (0xC0000000-based); masking with 0x3FFFFFFF
   handles both, but that is a defensive rule, not a verified equality.
7. **The HVS block-ID register's encoding.** Present at offset 0x08, reported by
   the driver for diagnostics, never interpreted. Identification uses the
   platform record and the SoC identity instead.
8. **The background fill colour.** The channel background register exposes the
   fill enable but no colour field on this generation, and the driver never
   writes a colour, so the fill's actual value is the hardware default. It is
   not used as a design element — the desktop pattern is a plane — but a
   bring-up check should record what colour appears where no plane reaches.

---

## 16. Sources

**Primary documentation**

* *BCM2835 ARM Peripherals*, Broadcom Europe Ltd., 06 February 2012.
  §1.1 — the enumerated ARM-accessible peripheral set and the statement that
  GPU-controlled peripherals are omitted and not recommended for ARM access.
  §1.2.3 — ARM physical address layout; RAM's bus addresses map to the uncached
  bus range starting at 0xC0000000. §1.2.4 — peripheral bus 0x7Ennnnnn
  corresponds to ARM physical 0x20nnnnnn on the 256 MiB part.
  §7.2 — the three interrupt pending registers and the two GPU pending
  registers (GPU interrupts 0–31 and 32–63). §7.5 — ARM interrupt register
  base 0x7E00B000, basic pending at 0x200, IRQ pending 1 at 0x204, IRQ pending 2
  at 0x208. The document has no mailbox, display or VideoCore chapter.
* *VideoCore IV 3D Architecture Reference Guide*, `VideoCoreIV-AG100-R`,
  Broadcom, 16 September 2013. §3 — QPU register map and instruction set.
  §9 — control lists, control record identifiers and primitive formats.
  Appendix B, Table 95 — V3D register base address 0x7ec00000 for BCM2835.
  Errata table — QPU and tile-buffer restrictions.

**Vendor firmware interface documentation**

* The VideoCore firmware's mailbox property interface documentation: channel 8
  as the ARM-request/property channel, the message and tag encoding, request
  and response code conventions, and the identifiers and semantics of the tags
  listed in section 4.4 (framebuffer geometry, allocation, pitch, offset,
  overscan, palette, clocks, power, memory, board identification). Cited for
  protocol facts; the mailbox block itself is not in the datasheet.

**Mainline Linux, cited for facts about behaviour on this silicon**

* `drivers/gpu/drm/vc4/vc4_hvs.c` — HVS display-list SRAM at register offset
  0x2000 with a 16 KiB size, the 128-byte bootloader reservation and the
  rationale for it, per-channel control fields (enable, reset, active
  width/height), background fill enable, list installation and the active-list
  register as flip acknowledgement, and the HVS's role as fetch/scale/composite
  feeding a pixel valve.
* `drivers/gpu/drm/vc4/vc4_plane.c` — the display-list element word order and
  field layout, the format and component-order tables, alpha modes and
  premultiplication, the unity-scaling rule and the line-buffer requirement
  only when vertical scaling is used, the statement that planes are bounded by
  display-list space rather than by a fixed register count, and the cursor being
  an ordinary plane placed on top.
* `drivers/gpu/drm/vc4/vc4_regs.h` — register offsets and bitfield positions
  used in section 5.2 and 5.3. Used as a record of facts; the field names and
  tables in this document are written from those facts, not transcribed.
* `drivers/gpu/drm/vc4/vc4_crtc.c` — planes appended to the list in ascending
  z-order (first element at the bottom), the end-of-list word, use of the
  active-list register to confirm a flip, and the per-channel composite output
  buffer allocation read as top/base pixel indices.
* `drivers/gpu/drm/vc4/vc4_kms.c` — aggregate plane load summed against the
  achievable HVS core clock, so that an over-subscribed scene is refused rather
  than allowed to underflow.
* `drivers/gpu/drm/vc4/vc4_txp.c` — the transposer as a writeback of the
  composed HVS output: destination pointer and pitch registers, raster
  alignment, and its use as a writeback connector.
* `drivers/gpu/drm/vc4/vc4_v3d.c` — the V3D clock and reset are obtained and
  enabled from the ARM.
* `arch/arm/boot/dts/broadcom/bcm283x.dtsi` — HVS at bus 0x7e400000, size
  0x6000, GPU interrupt 33; transposer at 0x7e004000, size 0x20, GPU interrupt
  11.
* `arch/arm/boot/dts/broadcom/bcm2835-common.dtsi` — pixel valves at
  0x7e206000, 0x7e207000 and 0x7e807000; HDMI at 0x7e902000; DPI at 0x7e208000;
  V3D at 0x7ec00000 with its clock from the clock manager and its reset from the
  power-management block.

**QEMU 11.1.0, cited for what the model does**

* `hw/arm/bcm2835_peripherals.c` — the modelled device set (no HVS, no pixel
  valve, a V3D placeholder), the four RAM aliases in the VideoCore bus address
  space, and the framebuffer and property channel placed on mailbox channels 1
  and 8.
* `hw/display/bcm2835_fb.c`, `hw/misc/bcm2835_property.c` — the single linear
  framebuffer and the implemented property tags.
* Measured: the probe results in section 10, and the memory tree reported by
  the monitor, which shows the peripheral container's leaf regions and the
  absence of any HVS device.

**This repository**

* `platforms/qemu_arm64.json` — machine, RAM, peripheral declarations, display
  geometry and the reserved display memory regions.
* `drivers/virtio-gpu/module.json`, `drivers/virtio-gpu/src/lib.rs` — the
  external-driver shape and the scan-out lifecycle this design extends.
* `crates/mac-video-interface/src/lib.rs` — the current video class contract.
* `system/src/machine/display.rs`, `system/src/machine/video.rs` — the resident
  front/back buffers, damage accumulation and the present path.
* `crates/mac-boot-screen/` — the software framebuffer path this platform
  currently lacks a device for.
* `docs/CURSORS.md`, `docs/DRIVERS.md`, `docs/DEVICE-TREE.md`,
  `apps/finder/src/dragging.rs`, `crates/mac-toolbox/tests/finder_drag.rs` — the
  cursor service, the driver model, the device-tree contract (including the
  window-versus-inventory-page rule of section 2) and the observable markers
  used in section 6.4.
* `tools/mac-build/src/platform.rs` — the build-time check that a declared
  peripheral window sits inside the MMIO range that claims it.
