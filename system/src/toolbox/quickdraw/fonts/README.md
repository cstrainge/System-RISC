# Resident Font Manager

The resident System owns the font catalog and payloads. Applications submit a
family, point size, face and UTF-8 text through System services; font files are
not application dependencies. `FontManager::take_resources` moves the font
resources out of the decoded System fork, avoiding another copy of their data.
The manager is immutable after initialization and reparses borrowed font tables
on demand without storing self-referential pointers.

The original System 7.6.1 font bundle contains nine families, 17 scalable faces
and the installed bitmap strikes: Chicago, Geneva, Monaco, New York, Palatino,
Times, Helvetica, Courier and Symbol. Its 83 FOND/FONT/NFNT/sfnt resources occupy
1,227,154 bytes. `scripts/extract_system7_fonts.py` packages the previously
inventoried immutable System/ROM/suitcase resources. It remaps colliding local
resource IDs and rewrites FOND associations and affected table offsets.
`provenance/system7-font-bundle.json` retains original and generated hashes.

`FontSelection` carries `family`, `size` and `face`; `FontMetrics` returns ascent,
descent, leading and maximum width. `catalog`, `family_id` and `family_name`
support discovery. `metrics`, `text_width` and `draw` prefer exact bitmap strikes;
the corresponding `_with_preference` methods also accept `OutlinePreferred`
and diagnostic `OutlineUnhinted`. Sizes 0 and family 1 select the classic
12-point default and Geneva application-font alias respectively. Point sizes
1 through 512 and classic face bits 0 through 6 are accepted.

Bitmap drawing preserves original signed bearings, Mac Roman mapping, advances
and baseline metrics. The original Chicago 12 and Geneva 9 rendering remains
unchanged. Matching styled resources take precedence over geometric synthesis.
Scalable drawing maps Unicode cmaps or classic Macintosh/Roman cmaps, supports
simple and compound glyphs, and runs the original `fpgm`, `prep`, `cvt ` and glyph
instructions through Skrifa's interpreter using `Target::Mono`. A Rust
nonzero-winding center sampler converts the hinted outline to monochrome ink.
The parser and rasterizer preserve the Canvas origin and clip.

The implementation uses the safe, `no_std` configurations of
[ttf-parser](https://docs.rs/ttf-parser/0.25.1/ttf_parser/struct.Face.html) and
[Skrifa](https://github.com/googlefonts/fontations/tree/main/skrifa), with `libm`
for soft-float RV64IMAC. Macintosh cmap interpretation follows Apple's
[TrueType reference](https://developer.apple.com/fonts/TrueType-Reference-Manual/RM06/Chap6cmap.html).

## Original-system verification

`python3 scripts/check_quickdraw_fonts.py` renders the same text, baselines,
families, sizes and faces as the isolated original `QuickDrawProbe`. It reads
the actual generated `System.rsrc`, checks all 83 bundled payload hashes, then
writes image, difference, metric and source records to
`results/system7/font-comparison-checks.json` and `results/system7/fonts/`.

Current observations:

| Comparison | Result |
|---|---|
| Ten default bitmap rows | All 256,000 RGB pixels and all measured metrics match |
| Ten forced TrueType rows | All widths, advances and 40 FontInfo fields match; 294 of 256,000 RGB pixels differ |
| Nine Times 18 style rows | All measured widths and vertical metrics match; 585 of 230,400 RGB pixels differ |
| Unhinted diagnostic | 3,932 differing RGB pixels; six metric rows differ |

The bitmap pixels, all reported hinted metrics, and bounded original outline/
shadow glyph probes are strict test gates. Synthetic outlines use a full 3×3
dilation with the original ink removed; shadows use a 4×4 dilation extending
one pixel above/left and two below/right, also with a hollow interior.
Underlines skip the horizontal ink extent of descenders with one-pixel margins.
TrueType and synthetic-style pixel differences are recorded explicitly, not
treated as exact compatibility. The main remaining differences involve Apple's
compound-accent placement, original glyph dropout control, and synthetic
outline/shadow effects. Skrifa's public path API does not expose the executed
glyph's SCANCTRL/SCANTYPE dropout state. Automatic pair kerning, complex-script
shaping, and exact fractional-width mode are not implemented here.

Resident unit tests additionally execute the hint programs and render text from
all 17 original sfnt faces, test compound/accented glyphs and clipping, reject
malformed families and unbounded point sizes, and check original runtime metric
goldens. Host and RV64 builds use the same engine.
