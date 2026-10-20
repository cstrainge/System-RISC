# StuffIt archive volumes

Status: design/backlog, tracked as
[P16](CORE-TOOLBOX-ROADMAP.md#p16--stuffit-archive-volumes). Implementation follows
the P15 HFS-B cutover and the necessary filesystem mount interfaces. Planning
this backend does not move it ahead of the required native startup filesystem.

## Mount and application behavior

Double-clicking a `.sit` in Finder requests an archive mount through the public
File/Volume Manager service. System identifies and validates the container;
Finder does not contain a decoder. Publish an ordinary volume with a desktop
icon, directory windows and eject behavior. Standard File dialogs, Resource
Manager, ResEdit and native/legacy file APIs see ordinary files and both forks.
Mounting or inspecting an archive never executes code from it. Explicit app
launch uses the normal Process Manager compatibility and integrity checks.

The first implementation is read-only. Reject writes, rename, delete and metadata
changes with the normal read-only status; copying out to HFS-B or another writable
backend preserves both forks, type/creator, Finder flags and representable dates.
Do not extract the whole archive to a hidden temporary volume. Store any Finder
view preferences outside the read-only archive.

Mount identity follows the source volume/file identity, not its path. Opening an
already mounted source activates that volume; a copied archive is a distinct
source. Hold a checked read lease on the source, preventing replacement/truncation
or detecting changes before returning inconsistent data. The source volume cannot
be ejected while depended on. Normal eject reports busy while files/dependent
mounts are open; a future forced-eject path must cancel I/O and invalidate handles.
Renaming/moving a source must retain its identity or fail explicitly while leased.

## Filesystem driver and shared decoder

Use the [in-kernel driver contracts](DRIVERS.md), including DRVR classification
and generated DABI metadata. This is a file-backed filesystem backend, not a
synthetic block disk. Define a checked positional-read backing-file contract and
filesystem operations alongside the block-based filesystem contract. Capture an
owned backing lease and release manager locks before reading/decoding; recursive
File Manager calls while holding its global state must not deadlock.

A shared safe no_std crate (proposed `mac-stuffit`) owns container parsing,
indexing and decoding, with bounded fallible allocation and no platform I/O.
Linux inspection/fixture tools and the resident adapter reuse it. Evaluate source
licensing, supported formats, no_std suitability and memory/work bounds before
selecting a dependency or porting code. Primary reference implementations include
XADMaster's [classic parser](https://github.com/MacPaw/XADMaster/blob/master/XADStuffItParser.m)
and [StuffIt 5 parser](https://github.com/MacPaw/XADMaster/blob/master/XADStuffIt5Parser.m);
these are research inputs, not a decision to embed that implementation.

Freeze a tested container/codec matrix before claiming support. Qualify classic
StuffIt and StuffIt 5 independently, with precise supported methods. Unsupported
versions or required codecs reject the mount explicitly; never silently omit
entries. SITX, encrypted/password archives, segmented archives and outer wrappers
such as BinHex/self-extracting files require separate gates. Never run a
self-extractor to read its contents.

Index the catalog once, decode forks on demand, and cache bounded blocks or seek
checkpoints. Repeated scrolling/seeking must not restart full decompression.
Limit entry counts, nesting, lengths, expansion, total work and cache memory;
check offsets/overflows and available integrity fields. Executable content must
finish applicable integrity checks before code entry. Schedule decoding outside
the UI/input path, with cancellation and bounded work slices.

Decode names using each format's encoding rules. Preserve original metadata,
then apply the native NFC, name-length and canonical caseless lookup contract.
Define collision/error behavior before publication: no silent merge, truncation,
path traversal or escape outside the volume. Reject an archive whose names cannot
be represented unambiguously by the initial backend. Legacy name/date projections
use the existing adapter rules, not a second set of conversion routines.

All runtime mount, node, fork and request IDs use the shared generation-tagged
facility and separate ownership checks. Parser entry indices are internal data,
not public handles. Eject and subsequent slot reuse must reject stale IDs.

## Acceptance

Canonical assertions live in `crates/mac-toolbox/tests` through the public SDK:
mount/open/enumerate/read both forks, Standard File/Resource Manager/ResEdit access,
read-only errors, copy-out metadata and fork equality, repeat-open activation,
distinct copied sources, source leases, busy eject and stale/foreign IDs after
eject/reuse. Exercise source rename/change/removal and backing I/O failures.

Use independently produced StuffIt/Expander fixtures and recorded decoded fork
hashes/metadata, covering every claimed format/codec. Add malformed lengths,
integrity failures, expansion limits, naming collisions and backwards/random
seeks. Parser tests supplement SDK tests. Verify responsiveness during decoding
and browsing with the Cursor Test and present/event traces; repaint only changed
view bounds. Keep original reference archives; prune obsolete generated test disks.
