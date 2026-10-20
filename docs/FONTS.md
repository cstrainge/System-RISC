# Installed fonts, OpenType and color glyphs

Status: design/backlog, tracked as [P9](CORE-TOOLBOX-ROADMAP.md#p9--system-fonts-folder)
and [P10](CORE-TOOLBOX-ROADMAP.md#p10--opentype-and-color-fonts).
The resident Font Manager currently takes the bundled FOND/FONT/NFNT/sfnt data
from System at startup. Installed-folder discovery, modern layout, CFF rendering
and color-font rendering are not established by those existing bitmap/TrueType
tests. See [current implementation and evidence](../system/src/toolbox/quickdraw/fonts/README.md).

## Fonts folder and discovery

Create a real Fonts directory inside the blessed System Folder when building
the startup volume. Resolve it relative to the selected installation's volume
and folder identity, never a hardcoded disk label or another mounted System
Folder. Expose native typed folder lookup and the original
[FindFolder Fonts selector](https://dev.os9.ca/techpubs/mac/Toolbox/Toolbox-472.html),
`kFontsFolderType = 'font'`, through the appropriate legacy adapter. Missing
folder, creation requests and read-only-volume errors need explicit behavior.

Keep core UI fonts and P4's built-in emoji set in System as dependable fallbacks.
Fonts-folder content adds installable families; users need not rebuild System or
copy font bytes into each app. An empty, missing or damaged Fonts folder must
not prevent boot or remove Chicago/Geneva/Monaco and the emergency UI font path.
Use versioned JSON only for font settings/selection metadata; font payloads
remain their actual font files/resources.

Supported-source inventory:

- Classic font suitcases, including `FFIL` resource forks, with FOND associations
  and FONT/NFNT/sfnt payloads. A suitcase may contain several families and faces.
  Preserve source-local associations and provenance; do not flatten every file's
  resource IDs into a single unqualified map or modify installed files to fix IDs.
- Standalone native `.ttf`/`.otf` data-fork fonts and `.ttc`/`.otc` collections,
  with individual collection-face identities. Validate actual format/tables;
  filename extensions and Finder type codes are hints, not sufficient validation.
- Explicit supported table/outline profiles for OpenType below. PostScript Type 1,
  legacy printer companions, aliases and compressed web-font containers require
  separate adapters; do not advertise them from recognizing their filenames.

The System catalog records source volume/CNID, resource identity or data-fork
face index, family/style names and format capabilities. Resolve duplicate names,
family numbers and styles deterministically, preserving built-in defaults and
documented native precedence. Reconstruct classic family-number mappings at the
adapter boundary without colliding with reserved aliases or changing on-disk
FOND links. Legacy family numbers are compatibility values, not an excuse for a
new resident handle scheme: catalog/face/session objects use the shared
generation-tagged facility and separate owner/lifetime checks. Do not reassign a
live selection to an unrelated font when a source disappears.

Initially discover and validate at boot, publish an immutable catalog, and state
that installing/removing fonts requires restart. Add a supported refresh API
later: build a new snapshot off the drawing path, publish atomically, retain
payloads used by active layouts and retire generations/cache entries safely.
Monitor actual folder/catalog changes rather than rescanning per draw or input.
Separate changed family-list notifications from glyph-cache invalidation.

System owns shared payloads/table indexes and bounded caches. No filesystem I/O
or full font parsing occurs on each glyph draw. Bound font count, bytes, table
sizes, glyph programs and allocations; report a rejected font while retaining a
usable catalog. Loading font resources never executes INIT or other unrelated
code from a suitcase. Resource Manager reads and legacy font enumeration need
an explicit origin-aware view, not arbitrary changes to an app's resource chain.

Finder shows the Fonts folder and appropriate original folder/suitcase icons
from resources. Capture original install/drop routing and duplicate handling
before adding that behavior; a font dropped into the active System Folder should
be routed through the installation service rather than merged into System's fork.
Folder display alone is not font installation. Font pickers and legacy font-menu
enumeration must obtain their family list through resident services.

## OpenType outlines, layout and variations

Use the [OpenType specification](https://learn.microsoft.com/en-us/typography/opentype/spec/otff)
as the format contract. Create a per-table capability matrix and pin parser,
shaper and rasterizer dependencies with licenses and test-font provenance.
The existing safe ttf-parser/Skrifa dependencies are candidates to extend or
complement; current use of their TrueType paths does not prove other formats.

Plan independently testable work for TrueType outlines, CFF and CFF2 outlines,
collection indexing, cmap coverage including variation sequences, names/styles,
metrics and native family matching. Extend the native Rust font-selection/layout
interface for features and variation axes rather than squeezing them into classic
face bits or changing legacy trap records. Persist selections by durable font/
style/axis identity, not a transient runtime handle.

Add GSUB/GPOS/GDEF-aware shaping with script, language and direction, kerning,
ligatures and mark placement. Shaping must produce shared glyph runs and text
cluster mappings used by measurement, drawing, line breaking and TextEdit hit/
selection/caret behavior. Script analysis and bidirectional layout are explicit
dependencies, not delivered merely by parsing GSUB. Preserve a measured classic
MacRoman layout mode so new shaping does not change original System 7 widths,
pixel comparisons or byte-based legacy editing semantics.

Support variable-font defaults, named instances and bounded axis coordinates,
including outline and metric changes; follow the
[variation model](https://learn.microsoft.com/en-us/typography/opentype/spec/otvaroverview).
Cache keys must include face/source generation, size, features, script/language/
direction, variation coordinates and rendering state. Distinguish an unsupported
optional table from a malformed font, with deterministic fallback and no false
claims of full OpenType coverage.

## Color-font rendering

Color glyphs use the same resident font selection/layout path, including P4 emoji
fallback. Prefer the selected font's usable glyph/sequence; use the built-in set
when it is missing. Installing a color emoji font is not a requirement for the
built-in fallback to work. Non-emoji color glyphs are supported by the same path.

| Format | Planned rendering contract |
| --- | --- |
| [COLR/CPAL](https://learn.microsoft.com/en-us/typography/opentype/spec/colr) | Version 0 layers and palettes first; version 1 paint graphs, transforms, gradients, composition and variation support as a separate gate. Bound traversal and handle foreground-color references correctly. |
| [CBDT/CBLC](https://learn.microsoft.com/en-us/typography/opentype/spec/cbdt) | Indexed color bitmap strikes, offsets/bearings, supported image encodings, scale selection and bounded decode. Reuse resident image codecs where applicable. |
| [sbix](https://learn.microsoft.com/en-us/typography/opentype/spec/sbix) | Bitmap strikes, origins, duplicate-glyph references and explicit encoding coverage. PNG support does not imply JPEG/TIFF support; qualify each encoding and fallback separately. |
| [SVG glyphs](https://learn.microsoft.com/en-us/typography/opentype/spec/svg) | A bounded implementation of the font SVG profile with transforms, palettes and clipping, no scripts or external fetches. Advertise the implemented profile only; an unimplemented glyph representation must not execute arbitrary content or masquerade as rendered. |

Define representation precedence when a glyph has outlines and multiple color
tables. Keep advances/baselines separate from ink bounds and preserve bearings,
alpha and palette choice. Test blending/clip/transform, monochrome destinations
and classic QuickDraw transfer modes; state which modes use color composition,
an outline/mask fallback or an explicit unsupported result. Do not synthesize
classic outline/shadow styles over color glyphs without a documented policy.

Bound image dimensions/decompressed bytes, outline/interpreter complexity and
paint/SVG graph work. Cache decoded/rasterized glyphs within a shared memory
budget; keep parsing/decoding out of per-event repaint loops and synchronize
mutations for app threads and future SMP. Include overhang in changed text damage
without repainting unrelated window areas. Larger color/variable fonts cannot
justify a full-scene redraw or unbounded startup memory growth.

## Delivery and acceptance

1. **F1 — Installation/catalog:** disk builder creates Fonts; boot discovers
   classic suitcases and publishes an origin-aware catalog. Expose folder lookup
   and family enumeration; retain the current System bundle as fallback.
2. **F2 — OpenType files/outlines:** standalone files and collections, native
   selection, TrueType/CFF outlines, then CFF2/variable-font metrics and defaults.
   Keep every unsupported capability visible in the matrix.
3. **F3 — Native shaping:** shared glyph runs, feature/axis selection and text
   cluster maps; integrate measurement and TextEdit while retaining classic
   rendering contracts. P4 emoji sequences use the same fallback/run machinery.
4. **F4 — Color:** COLR v0 and bitmap profiles, followed by COLR v1 and font SVG
   profile coverage. Add color/monochrome/palette/size fixtures and a QuickDraw
   demo showing installed OpenType fonts and both emoji and non-emoji color text.
5. **F5 — Live updates and installation UI:** transactional catalog refresh,
   safe in-use removal/replacement and original Finder install/drop behavior.
   Keep restart-required behavior explicit until this lifecycle gate passes.

Canonical conformance tests belong in `crates/mac-toolbox/tests`, through public
SDK font/resource/file services and real resident rendering. Use private boot
disks with Fonts present/missing/empty, several suitcases, colliding resource and
family IDs, loose OpenType files, collections, corrupt tables and oversized
payloads. Verify another mounted System Folder is ignored, sources survive
rename/move as specified, and duplicate enumeration/lookup is deterministic.
Test two apps, stale/foreign font objects and snapshot/cache lifetime on refresh.

Retain original System 7.6.1 font installation/enumeration captures and existing
pixel/metric goldens. Modern OpenType/color fixtures need separately pinned fonts
and independent reference-rendering provenance; the original OS is not an oracle
for these new capabilities. Cover CFF/CFF2, shaping/variation clusters, measured
advances versus drawing, palette changes, transparent overlap, clipping, malformed
paint graphs and every advertised color encoding. Measure boot cost, warm/cold
glyph latency, cache bounds, local-edit repainting and Cursor Test responsiveness.
Disk fixtures follow the existing cleanup rules; evidence does not require
retaining old generated HD images.
