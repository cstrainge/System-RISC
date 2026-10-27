Pinned upstream source: skrifa 0.48.0, crates.io archive SHA-256
`a5213b4825bac3587046330052d61bfac7bd3bd0d203c758d7c2ef726fcea034`. Original licenses and source headers are retained.

This local copy is used to make resident font allocation admission fallible.
The System RISC toolchain is pinned to nightly; skrifa uses allocator_api for
fallible Box admission. Upstream raster/hint arithmetic and glyph instructions
are preserved. Tests of the System integration live in mac-toolbox and compare
original Macintosh pixels as well as allocation refusal/retry.

Local patch: the embedded interpreter's normalized coordinates, subfont table,
boxed glyf instance, persistent scalar/definition/twilight arrays and initialization
stack reserve fallibly. Refusal uses DrawError::InsufficientMemory. The System
supplies separately admitted glyph scratch through DrawSettings::with_memory,
avoiding the upstream internal allocation path. No glyph instructions, hint
arithmetic, path conversion or raster rules are changed. Reconfiguration is not
advertised as an atomic transaction; the resident service constructs an unpublished
new instance from one retained font and never mixes collections across it.

The separately selectable auto-hinter and VARC internals are outside this patch's
allocation audit. The System explicitly selects the embedded interpreter, and
rejects unaudited preferred VARC/CFF/CFF2 source selection. The pinned original
System fonts use glyf TrueType outlines. This is not a
claim that all upstream APIs, including unused auto-hinting/color paths, have
been audited for allocation failure.

Trailing whitespace in source comments and extra final blank lines are normalized.
