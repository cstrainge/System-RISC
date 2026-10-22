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


## StuffIt 5 and Arsenic vectors

`arsenic-v5.sit`, `arsenic-randomized-v5.sit` and `arsenic-bad-crc-v5.sit`
contain only project-owned test bytes. Regenerate with:

```sh
python3 generate_sit5.py /path/to/stuffit-0.4.0/src/lib.rs .
```

The script pins the reference source SHA256 and compiles the independent
MIT/Apache-2.0 [stuffit 0.4.0 writer](https://crates.io/crates/stuffit/0.4.0) privately.
It explicitly adds the omitted Arsenic CRC32 trailer, encoded lookahead zeros
and final lookahead padding; one variant also randomizes the RLE block before
BWT. The bad-CRC variant changes only the encoded data-fork checksum. These
are synthetic vectors from an adapted independent writer, not original-Mac
observations. The header construction is separate from the production parser.

Each archive holds Arsenic with 34,384 data bytes and 1,038 resource bytes. Data:
32,768 xorshift32 bytes (seed 0x12345678, shifts left13/right17/left5, emit low
byte), then 1,100 bytes 0x90, four A bytes and two cycles 0..255. Resource:
ASCII `Resource fork` plus CR, then four cycles 0..255. They exercise repeated
bytes, MTF zero runs, multiple BWT blocks and a full 32 KiB transform.

Plain-data SHA256:
`6e7b57c98139fb9ffe61de1e0cfc0f88793572978138142e3d07ef64a8a61353`.
Resource SHA256:
`d0617102bb9a198c19fb75ddd77eb8bb1e7c09948ce99398ec781360e39ebf09`.
The expected ISO CRC32 values are 0xae216433 and 0xd9ee79cb respectively.

## Method 13 vectors

Regenerate the small project-owned method-13 archives with:

```sh
python3 generate_method13.py --reference /path/to/stuffit-0.4.0/src/lib.rs
```

The Python token encoder exercises all five standard codebooks, shared and
separate dynamic tables, every meta-length operator, overlapping matches, and
the zero-initialized history before the first literal. Both classic revision-2
and StuffIt-5 containers hold each variant. All normal variants expand to:

- Data: three zero bytes, eight cycles 0..255, byte 0x90, 100 X bytes, then `!`
  (2,153 bytes).
- Resource: the first 414 bytes of repeated ASCII `Resource fork` plus CR,
  followed by 0x00 and 0x90 (416 bytes).

`method13-independent-v5.sit` uses the unmodified method-13 compressor from the
published MIT/Apache-2.0 stuffit 0.4.0 source; the generator pins its SHA256 and
compiles it privately. It compresses the same project-owned contents with its
own match finding. The standard codebooks retain sit-algos Apache-2.0 provenance
in THIRD-PARTY.md. These are synthetic format vectors, not original Macintosh
producer observations.

`method13-window-v5.sit` expands to 99,969 cyclic bytes 0..255 and the normal
resource fork. It exercises the complete 64 KiB history wrap, distance 65,536,
length boundaries 64/65/1,088 and maximum match length 32,832.
`method13-bad-crc-v5.sit` changes only the declared data checksum; its resource
fork and catalog checksums remain valid, allowing recovery to be tested.
