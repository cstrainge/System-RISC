# Classic StuffIt parser fixtures

These are test inputs, not generated system media. All archived contents were
created for this project. No third-party applications or proprietary data are
included.

The archives were written on Linux with the independent StuffIt 1.5.1-compatible
writer [thecloudexpanse/sit](https://github.com/thecloudexpanse/sit), pinned to
commit `821ae5ea6d0c76ab83ac0942e12f398aa5944250` (BSD-2-Clause; its compress
implementation retains Berkeley's BSD license). `sit.h` preserves the original
Raymond Lau 1.5 documentation. Build it with `make sit`.

Run the writer from each input directory:

```sh
sit -T TEXT -C TEST -o nested-v1.sit Top
sit -T BINA -C TEST -o lzw-reset-v1.sit Reset
sit -T APPL -C TEST -o forks-v1.sit Forked
```

Input mtimes were Unix `1700000000`. Linux creation dates come from ctime;
regenerating later changes headers and their CRCs, but not fork payloads.

- `nested-v1.sit`: Top/Sub/Pattern is `bytes(range(256)) * 32`;
  Top/Café is `Native MacRoman name\r` (actual CR); Top/Stored is the
  concatenation of SHA-256 digests of UTF-8 `System RISC fixture {i}` for
  `i = 0..127`. The latter's uncompressed bytes are in `stored.bin`.
  The archive header counts one top-level item, despite seven entry headers.
  Folder length hints are nonzero. All nonempty forks use method 2; the writer
  keeps LZW even when incompressible input grows.
- `lzw-reset-v1.sit`: Reset is 65,536 cyclic bytes `0..255`, then 131,072
  xorshift32 bytes (seed `0x12345678`; shifts left13/right17/left5, modulo u32;
  emit low byte), then 131,072 cyclic bytes. The stream reaches 14-bit codes
  and includes CLEAR resets. Prepending `1f 9d 8e` to its payload and running
  GNU `gzip -dc` independently reproduced the complete 327,680-byte input.
- `forks-v1.sit`: Forked is `Data fork\r`; Forked.rsrc is
  `bytes(range(256)) * 8`. Checks nonempty resource/data fork ordering, APPL
  metadata and per-fork CRCs.

The unit tests construct RLE90 and Huffman format vectors separately. Those
vectors are not represented as output captured from an original Macintosh.
Original StuffIt 1.5.1 producer/runtime verification remains an integration gate.
