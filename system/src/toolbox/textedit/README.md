# Resident monochrome TextEdit

The application-facing `toolbox-textedit` crate contains synchronous trap
bindings only. System owns editor text, selection, wrapping, hit testing and
font advances. Drawing lives in the resident Desktop runtime child module
and uses the existing resident QuickDraw and Font Manager.

The first compatibility profile is plain, left-aligned Chicago 12 with its
original sixteen-pixel line height and twelve-pixel ascent. Original System
7.6.1 probes freeze line starts, points, mouse offsets, carriage returns,
wrapping spaces, tab/LF glyph advances, insertion, deletion and cursor keys.
Canonical assertions and their captured oracle live in
`crates/mac-toolbox/tests/conformance/textedit.rs` and `tests/textedit.rs`.

Original details preserved include:

- CR ends a line; LF and tab are ordinary glyphs. CRLF is preserved byte for byte.
- Empty text has zero recorded lines, and a trailing CR adds no recorded line.
- `set_text` selects the end. Reversed `select` ranges collapse at their end.
- `insert` inserts before the selected range and shifts that range. Typing a key
  replaces the selected range. `delete` with an insertion point does nothing.
- `point` uses the original `TEGetPoint` coordinates: the horizontal coordinate
  is one pixel after the drawing pen, and the vertical coordinate is the line
  bottom. It is deliberately not the inverse of mouse `offset` at a line edge.
  Visible caret drawing uses the insertion row, including the empty final line
  after CR; it does not reuse the classic point query for that case.

Native `move_cursor` adds Left/Right/Up/Down, visual LineStart/LineEnd and
DocumentStart/DocumentEnd, with an optional fixed selection anchor. Shift moves
the focus while preserving that anchor, including direction reversal. Vertical
movement remembers the desired horizontal position across short lines. Wrapped
line ends keep caret affinity; a trailing CR has a native empty final line.
Original `key`/TEKey arrow behavior remains unchanged. Finder maps Shift+arrows,
Home/End, Shift+Home/End, Ctrl+Home/End and Ctrl+Shift+Home/End onto this API.

Native Rust strings contain UTF-8, and native selection/line/point offsets count
UTF-8 bytes. Every supplied offset must fall on a character boundary. Full
Unicode storage is supported, while the initial original bitmap strike draws
its missing-glyph fallback for characters outside its repertoire. This profile
is not a Unicode shaping engine. Rectangles use checked signed sixteen-bit
coordinates, and layout hit/point results saturate at that coordinate boundary.

Native Copy, Cut and Paste operate directly on the System clipboard. They are
explicit native integration operations, distinct from original `TECopy` and
`TEPaste`, which use the private TextEdit scrap. Empty native Copy/Cut preserves
the clipboard; Paste with no TEXT flavor reports the Scrap Manager's missing
flavor error. Cut and Paste stage all fallible text/layout allocation before
publishing any mutation. Pascal string convenience traps convert textual
arguments and results but retain native UTF-8 offsets. Original TEHandle layout,
MacRoman byte indices, private scrap and A-line adaptation remain deferred.

The SDK offers owned `Editor` and borrowed `EditorRef` wrappers; `Drop` releases
an owned editor, and explicit `dispose` remains available to report errors. IDs include a slot generation. Exhausted
sixteen-bit generations retire their slot, so stale IDs never alias replacements.
The native profile bounds text at 32767 UTF-8 bytes and resident editors at 64.
Text and line-layout allocations use fallible reservation. Drawing stages its
new commands before publication, while port/region clones retain the existing
QuickDraw allocation behavior. An unsuccessful edit preserves the
previous text, selection and layout. No client pointer survives a request.

Each application context owns its manager; System serializes service dispatch
under the existing Desktop guard and uses the shared generation-tagged table.
The manager owns transferable Rust data. This does not claim MMU isolation.
Styled TextEdit, alignment modes, word selection, double-click tracking,
autoscroll and general editable Dialog Manager items remain separate work.

Native display options default to ruler off, wrapping on and numbers off, with
unchanged classic geometry/pixels. The additional versioned request records
leave original EditorInfo and classic TE semantics intact. The rich SDK exposes
`DisplayOptions`, `Ruler`, capability/extended-geometry queries and two-axis
scrolling on owned and borrowed editors. Rulers retain columns or pixels across
font changes; the classic Monaco strikes have special narrower non-ASCII symbols,
so column classification checks printable ASCII advances rather than trusting
only fixed-pitch metadata. See Apple's [font type specification](https://leopard-adc.pepas.com/documentation/mac/Text/Text-251.html)
and [TrueType post specification](https://developer.apple.com/fonts/TrueType-Reference-Manual/RM06/Chap6post.html).

Edits prepare a virtual splice and rewrap only until cached starts resynchronize;
unchanged suffix lines shift offsets/logical numbers without measuring glyphs.
Visible retained text and port snapshots are shared across line commands. Native
`blink` changes the retained caret and XORs cached/screen pixels inside its exact
clip, respecting window occlusion; it does not replay text. Pending scene damage
uses the same bounded caret damage through the compositor. Normal typing has no
UART logging. Private checks opt in to `input-trace` and resident trace code0
records (rewrapped bytes, snapshots, emitted commands and XOR bounds).

Canonical coverage: `tests/textedit.rs`, `textedit_display.rs`,
`text_edit_documents.rs`, `textedit_graphical.rs`, `typing.rs` and
`textedit_performance.rs`. Performance/graphical cases use private optimized
QEMU images; Rust owns assertions, capture scripts only transport evidence.
