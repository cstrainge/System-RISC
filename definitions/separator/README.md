# Separator control definition

System CDEF 29 (`CDEF_SEPARATOR`), classic procedure 464, variation 0.
This initial definition draws a one-pixel rule in the current theme's separator
role. Flags zero selects horizontal; `SEPARATOR_VERTICAL` selects vertical.
Both forms are passive: hit testing always returns `Part::None`, with no focus,
selection or tracking. Enabled/disabled instances draw the same grouping chrome.

Use the public Toolbox `controls().create_standard` with `StandardKind::Separator`,
`StandardState { bounds, ..Default::default() }`, and the desired reflow policy.
Set `state.flags = controls::SEPARATOR_VERTICAL` for a vertical rule. The existing
managed Control owner/ref provides drawing, configuration and disposal.

Reflow reports a one-pixel minimum on both axes and the creation size as the
preferred size. The line spans its assigned long axis without adding margins.
Drawing is clipped to its own bounds. Invalid flags, inverted bounds and
interactive state are refused with `BadArgument`. Unsupported variations,
nonempty labels, tracking, grid/container operations and bounds-delta messages
return typed `NotSupported`; no label is silently discarded. Labeled rules,
Geneva text/rotation and replacement-theme etched chrome remain future work.

This is a real independently loaded definition, installed in the System resource
fork and inspectable by the resource editor, not a second resident renderer.
