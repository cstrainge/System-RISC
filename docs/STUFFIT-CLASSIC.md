# Classic StuffIt filesystem

The shared `mac-sit` crate is the sole classic SIT parser/decoder. It is
`no_std` and forbids unsafe code. The filesystem provider will consume it
through resident-owned backing-file leases; there is no host-path interface.
See [FILESYSTEM-INTERFACE.md](FILESYSTEM-INTERFACE.md).

## Scope and format evidence

Only `SIT!` / `rLau`, container version **1**, is admitted. Later classic
containers, SIT5 and SITX are rejected, without speculative format probing.
The original 1.5 documentation survives in the independent
[sit writer's sit.h](https://github.com/thecloudexpanse/sit/blob/821ae5ea6d0c76ab83ac0942e12f398aa5944250/sit.h).
Committed fixtures and reproduction details live in
[mac-sit/tests/fixtures](../crates/mac-sit/tests/fixtures/README.md).

Corrections established against that documentation and writer output:

- The archive header counts **top-level** items, not all entry headers.
- Begin-folder sentinels have both methods 32. Their length fields can contain
  subtree hints; they do not describe following fork payloads. Nested-folder
  metadata other than the name is unspecified. Only the data method 33 is
  required for end-folder; its other fields may be invalid.
- Both header and fork CRCs use reflected CRC-16/IBM, polynomial `0xa001`,
  initial zero. The check value for `123456789` is `0xbb3d`.
- Version 1 encryption uses bit `0x10`; encrypted and unsupported forks remain
  visible in the catalog and fail explicitly when opened for expansion.
- Names are STR63 MacRoman, decoded through `mac-text`. Catalog normalization
  and collision policy belong to the filesystem provider. Dates are checked
  conversions from u32 seconds to u64 microseconds, with the shared 1904 epoch.

The four decoder methods are stored, RLE90, LZW and Huffman. Method 1 repeats
the previous byte using escape `0x90` (zero escapes a literal marker; counts
2..255 add count-1 repetitions). It is not PackBits. Method 2 is headerless
block-mode compress with 14-bit maximum codes, little-endian groups of eight,
and group realignment at width changes/CLEAR. Method 3 serializes an MSB-first
preorder binary tree, with a one-bit leaf tag then eight symbol bits. These
wire semantics were cross-checked against MacPaw's primary implementations:
[RLE90](https://github.com/MacPaw/XADMaster/blob/master/XADRLE90Handle.m),
[StuffIt dispatch](https://github.com/MacPaw/XADMaster/blob/master/XADStuffItParser.m),
[Huffman](https://github.com/MacPaw/XADMaster/blob/master/XADStuffItHuffmanHandle.m).
No XADMaster implementation code is included in this crate.

## Bounded processing and integrity

`IndexBuilder` requests only 22/112-byte headers, skipping payloads via checked
packed lengths. Default catalog budgets are 4,096 nodes and 64 folder levels.
Unbalanced nesting, count mismatch, invalid names/ranges and header CRC errors
poison the builder; it cannot resume into success.

`Decoder::step` accepts borrowed input/output for one call. Each step consumes
and emits at most 4 KiB, allowing a provider to yield between chunks. LZW owns
a fixed 16,384-entry dictionary and bounded reverse stack; Huffman owns at most
511 tree nodes and 256 pending branches. No method allocates by fork length.
Input chunk boundaries are independent of code and tree boundaries.

Output is provisional until `Progress::complete`: both declared length and
uncompressed CRC must agree. Copy-out must abort its destination transaction
if a later read reports corruption. After failure a decoder rejects reuse.
The caller must report a transport EOF before the declared packed length as
truncation; a temporary short input chunk alone is not EOF.

## Verified coverage and remaining work

Host tests cover real writer archives, nested folders, MacRoman names,
nonempty resource/data forks, CRC corruption, one-byte input/output chunks,
LZW width transitions/reset, bounded full Huffman trees, RLE escapes and
malformed/encrypted/unknown methods. These tests do not yet prove a production
StuffIt mount, native copy-out, or original Macintosh runtime compatibility.
The provider, provider-backed source adapter and Finder double-click route
remain the next integration gates.
