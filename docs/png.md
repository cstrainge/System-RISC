# Resident PNG images

Apps use the aggregate `mac-toolbox` SDK. PNG decoding and compositing run in
System behind `mac_abi::traps::IMAGE` (`0x10015`); no decoder is linked into apps.

```rust,ignore
let images = system.quickdraw().images();
let info = images.info(png_bytes)?;
window_graphics.draw_png(png_bytes, Rect::new(10, 20, 106, 84))?;
```

`Images::draw_to_surface` takes PNG bytes, an exclusive RGB24 pixel slice, width,
height, destination rectangle and clip rectangle. `SurfacePort::draw_png` supports
borrowed definition surfaces. Window/world drawing uses the current port origin
and region clip. Window content owns decoded RGBA, so repaint neither borrows app
bytes nor decompresses again. The immutable decoded buffer is an ordinary owned
`Vec<u8>` and can move between threads; this API introduces no scheduling policy.

## Profile

Static **noninterlaced** PNGs support all legal color/depth combinations:
grayscale 1/2/4/8/16, RGB 8/16, indexed 1/2/4/8, grayscale-alpha 8/16 and RGBA 8/16.
All five standard scanline filters, contiguous split IDAT and palette/grayscale/
RGB `tRNS` transparency are supported. Transparent samples are compared at their
original depth, including the low byte of 16-bit samples. Conversion retains the
high byte of 16-bit samples and expands low-bit grayscale across 0..255.

Adam7, APNG and unknown critical chunks return `NOT_SUPPORTED` with image detail
`UNSUPPORTED`; animation is never silently reduced to its first frame. Ancillary
metadata is CRC checked and skipped without decompression or UTF-8 conversion.
ICC/gamma/sRGB color management, EXIF and text metadata are outside this profile.

Drawing uses nearest-neighbor scaling and straight-alpha source-over into an
opaque RGB24 destination. Each encoded channel is
`(source*alpha + destination*(255-alpha) + 127)/255`; alpha zero preserves the
destination. This does not claim linear-light or ICC-aware composition.

## Bounds and error contract

- Encoded input: at most 1,048,576 bytes and 4,096 chunks.
- Dimensions: at most 1,024 on either axis and 262,144 total pixels.
- Borrowed output surface: existing limit of 640x480 pixels.
- Peak fallible image buffers: at most 2,098,177 filtered scanline bytes plus
  1,048,576 RGBA bytes. The scratch buffer is freed before retaining the image.
- Retained RGBA counts toward the existing desktop command storage budget.

`ImageError` exposes System `status` and a precise `kind`: signature, truncation,
header, chunk, CRC, compressed data, palette, unsupported feature, configured
limit, allocation failure, destination, or service failure. Invalid data returns
`BAD_ARGUMENT`, unsupported formats return `NOT_SUPPORTED`, and size/reservation
failures return `NO_MEMORY` with distinct `LIMIT_EXCEEDED`/`ALLOCATION_FAILED`
details. Empty/inverted destinations and invalid pixel-slice dimensions are
rejected. Malformed decoding finishes before any paint/retain, so failed draws
leave pixels untouched.

`info` validates complete chunk framing and CRCs but deliberately does not inflate
pixel data. Its success does not certify a valid compressed stream. Drawing also
checks zlib checksum, exact output length and complete compressed-stream usage.

System pins the core-only, allocation-free
[minipng 1.0.0](https://docs.rs/minipng/1.0.0/minipng/) decoder. Its inflate API does
not expose a byte count, so System first validates the stream using
[miniz_oxide 0.9.1](https://docs.rs/miniz_oxide/0.9.1/miniz_oxide/inflate/core/index.html)
with allocation/std features disabled, then runs minipng's PNG/filter decoder.
This intentionally does two bounded inflate passes using the same scratch
allocation. It prevents short compressed streams from being accepted as a
zero-filled tail. Every owned buffer uses `try_reserve_exact`; neither decoder
allocates hidden image or metadata storage. Existing heap use can still cause a
recoverable allocation failure; bounded image support does not promise unlimited
heap availability.

## Tests and evidence

`crates/mac-toolbox/tests/conformance/png.rs` owns shared no-std acceptance:
23 independent valid sample cases covering all 15 color/depth combinations,
filter types, alpha/tRNS, split IDAT and ignored compressed metadata; exact
scaled/clipped alpha pixels; and eight malformed/unsupported atomicity cases.
`conformance::run_all` invokes it on both host and the real graphical ECALL
fixture. Host-only `png_images.rs` additionally checks JSON/compiled oracle
identity, all truncated prefixes, limits, ordering and deterministic repaired-CRC
compressed-payload mutations. All calls use the public aggregate SDK and actual
resident service.

Fixtures come from `scripts/generate_png_fixtures.py`, an independent Python
`struct`/`zlib` encoder with explicit source sample values. It also regenerates
the compiled no-std oracle. This is a new PNG service, so original System 7.6.1
pixels are not presented as a PNG reference. Dependency and fixture identities
are retained in the repository's `provenance/png-support.json`.
