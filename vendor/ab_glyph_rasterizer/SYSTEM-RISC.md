Pinned upstream source: ab_glyph_rasterizer 0.1.10, crates.io archive SHA-256
`366ffbaa4442f4684d91e2cd7c5ea7c4ed8add41959a31447066e279e432b618`. Original licenses and source headers are retained.

This local copy is used to make resident font allocation admission fallible.
The System RISC toolchain is pinned to nightly; skrifa uses allocator_api for
fallible Box admission. Upstream raster/hint arithmetic and glyph instructions
are preserved. Tests of the System integration live in mac-toolbox and compare
original Macintosh pixels as well as allocation refusal/retry.

Local patch: Rasterizer::with_buffer takes already admitted Vec<f32> storage,
checks width*height+4 and capacity, then initializes the array without allocating.
Refusal returns the original buffer. The System uses this constructor instead of
new; the upstream line/curve raster algorithms and SIMD selection are unchanged. This is not a
claim that all upstream APIs, including unused auto-hinting/color paths, have
been audited for allocation failure.

Trailing whitespace in source comments and extra final blank lines are normalized.
