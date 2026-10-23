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
for the current RV64 LP64 calling convention (RVA23 instruction profile). Macintosh cmap interpretation follows Apple's
[TrueType reference](https://developer.apple.com/fonts/TrueType-Reference-Manual/RM06/Chap6cmap.html).

## Planned installed and modern fonts

Planned installation and modern-font work is detailed in
[Fonts folder, OpenType and color glyphs](../../../../../docs/FONTS.md): boot-time
Fonts discovery (P9), modern outlines/shaping/variations and color tables (P10),
then live catalog refresh. The current immutable System-only catalog remains the
implemented baseline. The built-in emoji fallback below is the related P4 track.

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

## Planned built-in emoji fallback

Status: backlog; the current service renders glyphs individually and does not
provide emoji fallback or sequence shaping. Track this as
[roadmap P4](../../../../../docs/CORE-TOOLBOX-ROADMAP.md#p4--built-in-emoji-fallback).
Implement it in the resident font/text renderer so all native Toolbox text
clients share one implementation and one System-owned asset set.

- Prefer the selected font when it provides a usable glyph or shaped emoji
  sequence. Otherwise resolve supported emoji through a built-in fallback set
  bundled as System resources. Test actual coverage before substituting the
  selected font's missing-glyph box; do not replace ordinary supported text.
- Resolve whole emoji sequences, including skin-tone modifiers, variation
  selectors, flags, keycaps and zero-width-joiner sequences. Respect explicit
  text versus emoji presentation; characters such as digits must not become
  colored emoji merely because they participate in some emoji sequences.
  Pin the supported Unicode data version and asset repertoire at implementation
  time, following [UTS #51](https://www.unicode.org/reports/tr51/).
  Define deterministic behavior for unsupported sequences and invisible format
  characters without claiming complete Unicode shaping support.
- Select redistributable artwork with retained license, source/version and
  hashes. Document the resource packaging and supported sizes; include metrics
  and sequence lookup data. Load and share the immutable assets in System,
  rather than embedding a decoder or copies of the artwork in applications.
- Measurement and drawing must resolve the same runs and advances. Define
  baseline, ascent/descent, ink bounds, line spacing, scaling, color/alpha,
  monochrome destinations and transfer-mode behavior. Include ink overhang in
  damage and clipping. Preserve existing family-metric queries and classic
  text metrics; expose additional native layout information where needed.
- Native TextEdit caret movement, deletion, selection, hit testing and wrapping
  must respect emoji grapheme boundaries, using pinned
  [UAX #29](https://www.unicode.org/reports/tr29/) data. Preserve original UTF-8
  text in clipboard/save operations; rendered images are not replacement text.
  This requires shared layout/segmentation work, beyond glyph rasterization.
- Preserve legacy Pascal/MacRoman conversion and byte-based TextEdit contracts.
  Existing MacRoman pixels, widths, style effects and clipping remain regression
  gates. Emoji are a native Unicode extension, not a new interpretation of
  legacy byte values or an implicit private encoding.
- Keep the ordinary text path cheap. Index sequence coverage, cache bounded
  fallback glyphs/runs with size and rendering state in their keys, and avoid
  asset decoding on every draw. Synchronize shared cache mutation for concurrent
  app threads. Repaint only affected text/selection/caret bounds.

Acceptance lives in `crates/mac-toolbox/tests` through public SDK services:
selected-font precedence, fallback coverage, mixed text, sequence/presentation
cases, measure/draw agreement, baseline/clipping/scaling, color and monochrome
destinations, editing and lossless text round-trips, unsupported sequences,
concurrent clients and bounded cache use. Add a graphical TextEdit/QuickDraw
sample and retain existing original-system MacRoman comparisons. Measure warm
and cold rendering plus local-edit repaint cost; emoji must not introduce full
window repaints or degrade Cursor Test responsiveness.
