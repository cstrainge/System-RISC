# Resident monochrome TextEdit

The application-facing `toolbox-textedit` crate contains synchronous trap
bindings only. System owns editor text, selection, wrapping, hit testing and
Chicago 12 metrics. Drawing lives in the resident Desktop runtime child module
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

Editors require explicit `dispose`; the SDK has no destructor that can hide a
busy-service disposal failure. IDs include a slot generation. Exhausted
sixteen-bit generations retire their slot, so stale IDs never alias replacements.
The native profile bounds text at 32767 UTF-8 bytes and resident editors at 64.
Text and line-layout allocations use fallible reservation. Drawing stages its
new commands before publication, while port/region clones retain the existing
QuickDraw allocation behavior. An unsuccessful edit preserves the
previous text, selection and layout. No client pointer survives a request.

The current Desktop owns the foreground application's manager and serializes it
under its existing guard. The manager owns only transferable Rust data. A future
application scheduler must put a manager in each application context; this code
does not claim that process isolation or a multi-application scheduler exists.
Styled TextEdit, arbitrary font selection, alignment modes, word selection,
double-click tracking, autoscroll and editable Dialog Manager items remain to
be implemented and compared separately.
