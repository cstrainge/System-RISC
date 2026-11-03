# Static Text CDEF

`CDEF_STATIC_TEXT` is System resource `CDEF 18`, named **Static Text**.
It owns UTF-8 text drawing, font metrics, justification and clipping. The
resident Control Manager owns its generation-tagged identity and lifetime.
It never captures input or advertises a clickable part.

`Controls::create_static_text` returns the RAII `StaticText` owner; `set` skips
identical text and rejects over-capacity UTF-8 rather than silently truncating.
Left, centre and right justification are typed. A changed label invalidates
its own rectangle; it cannot invalidate its parent window. An explicit window
update can replay its retained drawing under the current damage/occlusion clip.

`STATIC_TEXT_PROCEDURE` is classic procID 288 (resource 18, variation 0).
The native UTF-8/state ABI remains separate from a classic 68k ControlRecord.
