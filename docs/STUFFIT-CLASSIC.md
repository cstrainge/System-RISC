# StuffIt formats and filesystem

The shared `mac-sit` crate is the sole SIT parser/decoder. It is
`no_std` and forbids unsafe code. The filesystem provider will consume it
through resident-owned backing-file leases; there is no host-path interface.
See [FILESYSTEM-INTERFACE.md](FILESYSTEM-INTERFACE.md).

## Scope and format evidence

Header-based dispatch admits classic container revisions **1** and **2**
(the latter covers StuffIt 1.6 through 4.5), and the **StuffIt 5** container.
Classic signatures include SIT!, SITD, SIT2 and SIT5; installer signatures STin,
STi3 and ST65 also use the classic catalog. The version-5 banner selects its
separate two-block layout. An archive's external format is independent of the
FSYS v2 driver interface. SITX and split/encrypted extraction remain unsupported;
errors retain the exact unsupported feature. The driver is not complete while
remaining methods and workflow gates are missing.
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

The current decoder methods are stored, RLE90, LZW, Huffman and Arsenic (15). Method 1 repeats
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

`IndexBuilder` requests 22/112-byte classic headers or bounded chunks (at most
4 KiB) of the variable-length version-5 headers, skipping fork payloads through
checked packed lengths. Classic revision 2 uses its declared catalog offset
and 0x80 encryption flag. Version 5 checks archive and entry header CRCs,
validates parent offsets through a sorted directory index, accounts for directory
sentinels without manufacturing files, and verifies declared child counts. Default catalog budgets are 4,096 nodes and 64 folder levels.
Unbalanced nesting, count mismatch, invalid names/ranges and header CRC errors
poison the builder; it cannot resume into success.

`Decoder::step` accepts borrowed input/output for one call. Each step consumes
and emits at most 4 KiB, allowing a provider to yield between chunks. LZW owns
a fixed 16,384-entry dictionary and bounded reverse stack; Huffman owns at most
511 tree nodes and 256 pending branches. No method allocates by fork length.
Input chunk boundaries are independent of code and tree boundaries.

Arsenic uses resumable arithmetic decoding, MTF, inverse BWT and RLE, including
randomized and multiple blocks. Its final CRC-32/ISO-HDLC replaces the unused
legacy per-fork CRC16 field. Every decoder step bounds consumed and produced
bytes to 4 KiB; CPU-only work such as histogram/transform construction yields
through `Progress::needs_input = false` rather than requesting unnecessary I/O.
Storage is bounded by the declared block (at most 2^24 symbols), further capped
by the known fork's RLE size bound. Tiny resource forks do not reserve the full
block. Fallible allocation reports the installed module's finite heap budget.

Version-5 layout/CRC evidence comes from the primary
[StuffIt 5 parser](https://github.com/MacPaw/XADMaster/blob/master/XADStuffIt5Parser.m)
and [Arsenic decoder](https://github.com/MacPaw/XADMaster/blob/master/XADStuffItArsenicHandle.m).
Randomization constants retain their Apache-2.0 provenance in
[THIRD-PARTY.md](../crates/mac-sit/THIRD-PARTY.md). The production codec contains no
synchronous std::io reader and no whole-fork extraction buffer.

Output is provisional until `Progress::complete`: both declared length and
uncompressed CRC must agree. Copy-out must abort its destination transaction
if a later read reports corruption. After failure a decoder rejects reuse.
The caller must report a transport EOF before the declared packed length as
truncation; a temporary short input chunk alone is not EOF.

## Error identity and recovery

The installed driver preserves archive errors through FSYS and File Manager:
truncation, signature, header/fork checksum, malformed data, configured limits,
encryption, unsupported archive/entry-header version and unsupported compression method. The SDK's
`files::ArchiveError` retains the version/method byte and supplies an explanation;
Finder uses resource strings rather than a generic I/O alert. Source transport
errors remain transport errors. Failed mounts must release leases so rename and
another open work immediately. See FORMAT.md for the error status allocation.

## Verified coverage and remaining work

Host tests cover real writer archives, nested folders, MacRoman names,
nonempty resource/data forks, CRC corruption, one-byte input/output chunks,
LZW width transitions/reset, bounded full Huffman trees, RLE escapes and
malformed/encrypted/unknown methods. These tests do not yet prove a production
StuffIt mount, native copy-out, or original Macintosh runtime compatibility.
The production `drivers/stuffit-filesystem` DRVR now passes a private native
public-SDK gate for HFS-B and writable/read-only host source files: nested mounting, Unicode lookup, bounded
reads/seek/EOF, both forks, data copy-out, read-only rejection, corrupt payloads,
distinct copies, source position independence and stale volume generations.
The default image installs it in the blessed Drivers folder. Module tests also
cover deferred short reads, cancellation acknowledgement and fresh callback
buffers. Full two-fork copy transactions, original Macintosh comparisons and
guest cancellation during a deferred provider-backed read remain
integration gates. See the [driver README](../drivers/stuffit-filesystem/README.md).


## Multi-version integration work

Host checks verify classic revision-2 offset/encryption semantics, version-5
variable headers, CRCs, parent/child validation, sentinels, long names, comments,
Arsenic randomization, multiple blocks, one-byte input/output fragmentation,
CPU-only yields, bad CRC, truncation, cancellation and immediate retry.
The private host verifier expanded and CRC-checked all nine nonempty forks of
the user's SuperPaint 3.5 reference archive. No proprietary archive bytes are
committed. Seven private optimized guest gates now pass: classic mounting,
version-2/5 public SDK reads, randomized two-fork Arsenic and CRC rejection,
writable/read-only raw-host sources, Finder normal/error recovery, driver removal,
and actual SuperPaint opening plus verification of all nine forks from both
host-export modes. These prove the named slice, not the remaining driver contract.

Remaining compression methods (including method 13), nested mounts backed by
the same provider, complete transfer transactions and original Macintosh
producer/runtime comparisons remain explicit completion gates. Graceful errors
for these cases do not close those gates.
