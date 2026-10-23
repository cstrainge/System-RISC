# Native TextEdit display and performance

System owns layout, hit testing, font advances, selection, caret presentation and
all display features. `toolbox-textedit`, re-exported by `mac-toolbox`, is a thin
Rust service interface. The original records and legacy operations keep their
meaning. Defaults remain wrap on, numbers off and ruler off.

The owned `Editor` and borrowed `EditorRef` expose `display_capabilities`,
`display_options`, `set_display_options`, `display_info` and `scroll_axes`.
`create_owned_with_options` disposes its new editor if configuring it fails.

```rust
use mac_toolbox::{Rect, textedit::{DisplayOptions, Ruler}};

let view = Rect::new(0, 0, 400, 240);
let options = DisplayOptions::default()
    .wrapping(false)
    .numbered(true)
    .ruler(Ruler::Column(80));
let editor = system.textedit().create_owned_with_options(view, view, options)?;
editor.set_text("first line\rsecond line")?;
let geometry = editor.display_info()?;
editor.scroll_axes(geometry.scroll_max_x, 0)?;
```

`Ruler::Column` retains its column count through font changes; `Ruler::X` retains
its pixel position in text space. Positive columns and nonnegative pixel limits
are validated before changing state. Capabilities report supported features and
version; unsupported versions/option kinds return typed `NotSupported`.

Line numbers count CR-delimited logical lines, including an empty insertion line
after a final CR. Only the first wrapped segment is numbered. The shaded gutter
grows at decimal digit boundaries; its rule and the ruler's shaded zone use the
same palette. The effective text view excludes the gutter. Horizontal extents
use cached wide advances; classic `LineInfo.width` keeps its existing saturation.
Option changes and resizing preserve the top visible text anchor. The vertical
range includes any top inset between the text destination and its view.

An app still erases/draws inside an owned window update or masked update after
editing, scrolling, activation or geometry changes. During idle, call
`editor.blink(window, ticks)`: System XORs the committed caret in place, keeping
retained drawing, cached pixels and occlusion coherent. A true result asks the
caller to provide a current drawing because the caret's retained state is stale.
The original `idle` remains a model-only operation. Blink is not a substitute for
an app drawing its text changes.

Text Edit's resource-defined Options dialog uses managed radio/button/checkbox
and static-text CDEF objects plus an owned TextEdit for numeric input. Changes
apply to the selected document and defaults for new documents. Cancel keeps the
original settings. Invalid limits use a System alert; persistence errors remain
visible. Strict preference JSON version2 stores font, size, wrapping, numbers
and ruler mode/value in the extensionless JSON/TEDT preference document. A rebuilt
installation recreates earlier preference schemas. The numbered gutter touches
the window's top, left and bottom edges, with text padding inside the view.

The document ceiling stays 32,767 UTF-8 bytes. This is plain TextEdit, without a
new styled-text or Unicode-shaping claim. The resident README records the classic
oracle and other remaining TextEdit work.

# Verification

Canonical tests live in `crates/mac-toolbox/tests`:

- `textedit`: unchanged original/layout/font/navigation/failure assertions.
- `textedit_display`: defaults/capabilities, typed refusals, wide horizontal
  geometry, ruler/font behavior, logical counts, inset scroll range and a
  700-edit Unicode/wrap oracle compared with full reflow.
- `text_edit_documents`: newline codecs, versioned preferences, content revisions
  and actual Finder/Open/Save/JSON document workflows.
- `textedit_graphical`: original reference pixels and native insertion carets.
- `textedit_performance`: actual app paths; identical bounded rewrap work in
  small/near-cap documents, zero typing logs, zero blink updates/configurations,
  no blink text/snapshot work and exact caret framebuffer copies with a visible
  or hidden mouse.

Checks opt in to resident tracing and `text-edit/input-trace` on private media.
`cargo xtask check` enables these diagnostics; normal release packages omit the
hot-path state/layout/batch messages. Full host assertions and focused guest
checks are distinct from claiming full classic-app/Toolbox compatibility.

Private evidence, including all timing observations and unsuccessful attempts,
is recorded under `results/system7/textedit-*` outside the repository. Timings
are QEMU observations, not fixed-host speed guarantees. The Text Edit viewport
is larger after removing its footer; whole-frame typing/scroll timing includes
those additional visible pixels. Trace work counts and exact caret damage are
the reproducible performance acceptance criteria.
