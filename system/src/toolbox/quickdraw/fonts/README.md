# Resident Font Manager

The resident System owns the font catalog and payloads. Applications submit a
family, point size, face and UTF-8 text through System services; font files are
not application dependencies. `FontManager::take_resources` moves the font
resources out of the decoded System fork, avoiding another copy of their data.
Each published manager is immutable. Replacement snapshots share immutable
payloads through fallibly allocated `Arc` storage and reparse borrowed rendering
tables without self-referential pointers.

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

## Installed-source snapshots

The manager admits classic/native suitcase resource forks and individual static
TrueType data-fork faces. Filesystem discovery, watches and publication belong
to the caller, not to the renderer. The caller pins the prior immutable manager:

1. `begin_installed_snapshot() -> Result<InstalledFontSnapshot, FontInstallError>`.
2. Once per maintenance turn, call `reuse_source(key, revision) -> Result<bool, _>`
   for an identity whose revision is known unchanged. If it returns false, load
   bytes and call `add_source(InstalledFontSource { key, revision, data })`.
3. Use `InstalledFontData::SuitcaseFork(&bytes)` for raw resource forks,
   `Suitcase(&resources)` for an already decoded fork, or `TrueType(&bytes)` for
   one standalone face. All bytes needed for later rendering are copied/moved
   into immutable storage during admission; borrowed inputs need not outlive it.
4. Report each `add_source` error. If `error.failure.is_source_rejection()` is
   true, that source is omitted and the builder remains usable. Its old numeric
   selections remain retained. Other errors poison the transaction.
   `finish() -> Result<FontManager, FontInstallError>` produces the full proposed
   snapshot only if no terminal error occurred. `reject()` also poisons it after
   caller-side I/O failures.
5. Recheck filesystem revisions and atomically replace the published manager
   only if its baseline is still the pinned prior manager. Otherwise discard the
   proposal and retry. Never parse or allocate under the publication gate.

`with_installed_snapshot(&[InstalledFontSource])` is a strict, all-at-once
convenience form returning the first rejection, not the scheduler-maintenance
integration. The incremental caller reports source errors and continues only
when the classification allows it; no diagnostics are silently swallowed.
One admission can still decode and hash up to 8 MiB: bounded work does not
establish a target latency guarantee.

Nonterminal source rejections are `SourceLimit`, `DuplicateResource`,
`Malformed`, `Unsupported`, `MissingName`, `MissingAssociation` and `NoFamilies`.
Terminal failures are `Allocation`, aggregate `Limit`, `DuplicateSource`,
`FamilyIdsExhausted`, `RevisionExhausted` and
`RejectedSnapshot`. A source limit is per-file/table/name/face capacity; aggregate
limits cover the transaction's source/work counts and retained storage/families.
Rejected attempts still consume source/work budget and their keys cannot be
retried within the same transaction. Their payloads/families are not installed.
Status projection reports `Malformed`, `DuplicateResource`, `MissingName` and
`MissingAssociation` as `CORRUPT_DATA`, not a caller argument/filename error.
`Unsupported` remains `NOT_SUPPORTED`; allocation and capacity failures remain
`NO_MEMORY`. The typed failure reason remains available independently.

Keys contain a `u64` volume identity and `u32` CNID. The volume identity and
revision must distinguish mounted-volume/file incarnations; a filename, mtime
with known collisions, or reused mount slot is insufficient for `reuse_source`.
Supplied bytes are authoritative: a changed payload at an already retained
key/revision receives new numeric selections while old selections retain their
old immutable bytes. Multiple retained payloads at one key/revision make
`reuse_source` return false rather than choose one arbitrarily.

`catalog()` and name-to-ID lookup show bundled and currently installed families.
Omitted or revised installed sources disappear from that list. Numeric family
IDs start at 256, skip every bundled/retained ID and are never recycled within a
manager lineage. Old selections, `family(id)`, `family_name(id)` and
`family_key(id)` continue using their original immutable payloads. No removed
font is silently rebound or replaced with another source.

Persist `family_key`, not the numeric ID. Bundled keys are `bundled:<family>`;
installed keys bind the canonical SHA-256 digest of the source-local font
resources, format and local FOND resource ID (or standalone face zero).
Keys are ASCII and shorter than 128 bytes. `family_id_for_key` searches only the
current catalog. Identical installed copies share a persistence key; lookup
chooses the first available source in volume/CNID order. Bundled names have
precedence, followed by installed volume/CNID/FOND-resource ordering.
`revision() -> u32` changes only when the available catalog changes and refuses
overflow rather than wrapping. FOND resource IDs, original classic family
numbers, source/revision, and standalone style names remain available on
`FontFamily`. Standalone files remain separate catalog families, even when their
family names match; styles are not silently combined across unrelated sources.

`resources()` and `bundled_resources()` expose only the bundled resource slice
for the legacy Resource Manager chain. `available_resources()` yields current
`(Option<FontSourceKey>, &Resource)` entries, preserving each source's original
resource namespace; `None` denotes bundled resources. `retained_resources()` is
renderer backing, including removed sources, not an enumeration/Resource Manager
namespace. No installed resource IDs or FOND bytes are rewritten.
Only FOND-associated payloads are interpreted as faces, once per source-local
resource. Unreferenced records remain inert; classic suitcases can contain short
legacy `FONT` family records that are not bitmap strikes. Every linked face
still undergoes the same bitmap/TrueType validation before enumeration.

Admission limits are 128 current sources, 256 retained source revisions, 8 MiB
per source, 64 MiB retained font payload/name bytes including bundled data,
1,024 resources per suitcase, 4,096 retained families, 4,096 newly parsed FOND
associations and 262,144 newly examined glyph-location entries per transaction.
Metadata counts, table count (128), glyph count (16,384 per face), names and maxp
capacities are also bounded. Allocations are fallible. Retention exhaustion
refuses refresh; it does not reclaim payloads beneath live selections.

Only static `glyf`/`loca` TrueType outlines are admitted for installed sfnt data.
Names support Unicode/Windows UTF-16BE and Macintosh Roman, with bounded English/
neutral preference and typographic-family/style fallback to legacy name IDs.
Collections, CFF/CFF2, VARC, variable fonts and color glyph profiles are explicitly
unsupported. Table ranges, duplicate tags, glyph-location ordering and declared
capacities are checked; safe renderer/interpreter bounds remain in force when
glyphs are used. Admission is not exhaustive proof of all glyph programs.
Ordinary malformed/unsupported/limit/allocation failures carry source identity
and a typed cause, leave the published manager intact, and do not modify files.

`FontManager::inspect_source(InstalledFontData) ->
Result<Vec<InspectedFontFamily>, FontInstallError>` uses the same bounded source
parser without installing anything, allocating selection IDs, or constructing
another manager. The returned owned metadata has public `name: String`,
`style_name: Option<String>`, `classic_family: Option<i16>` and
`fond_id: Option<i16>` fields. Inputs can be released when it returns. Inspection
errors have `source: None`; callers supply file identity in their own error
context. An inspection session's owner-bound lifetime, file I/O and UI belong to
the caller, not this parser.

Modern layout and additional outline/color formats remain separate work in
[Fonts folder, OpenType and color glyphs](../../../../../docs/FONTS.md).
Native loading, refresh, removal, retained rendering, allocation failures,
responsiveness and persistence round trips remain target-runtime qualification
gates. Compilation does not establish those properties.

## Original-system verification

`python3 scripts/check_quickdraw_fonts.py` renders the same text, baselines,
families, sizes and faces as the isolated original `QuickDrawProbe`. It reads
the actual generated `System.rsrc`, checks all 83 bundled payload hashes, then
writes image, difference, metric and source records to
`results/system7/font-comparison-checks.json` and `results/system7/fonts/`.

Historical observations (not current shipping-OS acceptance):

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

The historical host Rust suite is not evidence for native loading, allocation,
interrupt, scheduler or rendering behavior. No new Rust tests accompany the
installed-source implementation.

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
