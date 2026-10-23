//! Thin resident TextEdit bindings with owned editors and borrowed references.
//! Generation checks reject editor IDs after disposal or slot reuse.
#![no_std]
#![forbid(unsafe_code)]
use mac_abi::textedit::*;
pub use mac_abi::{Point, Rect, WindowId,
                  quickdraw::FontSelection,
                  textedit::{CursorMovement, EditorId, MAX_BYTES, MAX_EDITORS}};
pub use mac_traps::{Client, Error, Result};

/// Native editor snapshot. Boolean state and layout data do not expose requests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EditorInfo
{
    pub dest:             Rect,
    pub view:             Rect,
    pub selection_start:  u32,
    pub selection_end:    u32,
    pub text_length:      u32,
    pub line_count:       u32,
    pub line_height:      i16,
    pub ascent:           i16,
    pub active:           bool,
    pub caret_visible:    bool,
    pub revision:         u32,
    pub content_revision: u64,
    pub scroll_y:         u32,
    pub scroll_max:       u32
}
impl From<mac_abi::textedit::EditorInfo> for EditorInfo
{
    fn from(info: mac_abi::textedit::EditorInfo) -> Self
    {
        Self { dest:             info.dest,
               view:             info.view,
               selection_start:  info.selection_start,
               selection_end:    info.selection_end,
               text_length:      info.text_length,
               line_count:       info.line_count,
               line_height:      info.line_height,
               ascent:           info.ascent,
               active:           info.active != 0,
               caret_visible:    info.caret_visible != 0,
               revision:         info.revision,
               content_revision: info.content_revision,
               scroll_y:         info.scroll_y,
               scroll_max:       info.scroll_max }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LineInfo
{
    pub start: u32,
    pub end:   u32,
    pub width: i16
}
impl From<mac_abi::textedit::LineInfo> for LineInfo
{
    fn from(info: mac_abi::textedit::LineInfo) -> Self
    {
        Self { start: info.start,
               end:   info.end,
               width: info.width }
    }
}

#[derive(Clone, Copy)]
pub struct TextEdit<'a>
{
    client: Client<'a>
}
impl<'a> TextEdit<'a>
{
    pub const fn new(client: Client<'a>) -> Self
    {
        Self { client }
    }

    fn request(self, mut call: TextEditCall, input: &str, output: &mut [u8])
               -> Result<TextEditCall>
    {
        self.client.textedit(&mut call, input, output)?;
        Ok(call)
    }

    fn simple(self, id: EditorId, operation: u32) -> Result<EditorInfo>
    {
        Ok(self.request(TextEditCall { id,
                                       operation,
                                       ..Default::default() },
                        "",
                        &mut [])?
               .info
               .into())
    }

    pub fn create(self, dest: Rect, view: Rect) -> Result<EditorId>
    {
        Ok(self.request(TextEditCall { operation: CREATE,
                                       info:
                                           mac_abi::textedit::EditorInfo { dest,
                                                                           view,
                                                                           ..Default::default() },
                                       ..Default::default() },
                        "",
                        &mut [])?
               .id)
    }

    pub fn dispose(self, id: EditorId) -> Result<()>
    {
        self.simple(id, DISPOSE).map(|_| ())
    }

    pub fn copy(self, id: EditorId) -> Result<EditorInfo>
    {
        self.simple(id, COPY)
    }

    pub fn cut(self, id: EditorId) -> Result<EditorInfo>
    {
        self.simple(id, CUT)
    }

    pub fn paste(self, id: EditorId) -> Result<EditorInfo>
    {
        self.simple(id, PASTE)
    }

    pub fn set_rects(self, id: EditorId, dest: Rect, view: Rect) -> Result<EditorInfo>
    {
        Ok(self.request(TextEditCall { id,
                                       operation: SET_RECTS,
                                       info:
                                           mac_abi::textedit::EditorInfo { dest,
                                                                           view,
                                                                           ..Default::default() },
                                       ..Default::default() },
                        "",
                        &mut [])?
               .info
               .into())
    }

    pub fn info(self, id: EditorId) -> Result<EditorInfo>
    {
        self.simple(id, INFO)
    }

    /// Select the resident font for layout, hit testing and subsequent drawing.
    /// A failed selection leaves the editor unchanged.
    pub fn set_font(self, id: EditorId, font: FontSelection) -> Result<EditorInfo>
    {
        let mut call = TextEditFontCall { operation: SET_FONT,
                                          id,
                                          font,
                                          ..Default::default() };
        self.client.textedit_font(&mut call)?;
        Ok(call.info.into())
    }

    pub fn set_text(self, id: EditorId, text: &str) -> Result<EditorInfo>
    {
        Ok(self.request(TextEditCall { id,
                                       operation: SET_TEXT,
                                       ..Default::default() },
                        text,
                        &mut [])?
               .info
               .into())
    }

    pub fn insert(self, id: EditorId, text: &str) -> Result<EditorInfo>
    {
        Ok(self.request(TextEditCall { id,
                                       operation: INSERT,
                                       ..Default::default() },
                        text,
                        &mut [])?
               .info
               .into())
    }

    pub fn delete(self, id: EditorId) -> Result<EditorInfo>
    {
        self.simple(id, DELETE)
    }

    pub fn text(self, id: EditorId, output: &mut [u8]) -> Result<&str>
    {
        self.read(id, TEXT, output)
    }

    pub fn selection_text(self, id: EditorId, output: &mut [u8]) -> Result<&str>
    {
        self.read(id, SELECTION_TEXT, output)
    }

    fn read(self, id: EditorId, operation: u32, output: &mut [u8]) -> Result<&str>
    {
        let call = self.request(TextEditCall { id,
                                               operation,
                                               ..Default::default() },
                                "",
                                output)?;
        core::str::from_utf8(output.get(..call.transferred).ok_or(Error::BadArgument)?).map_err(|_|Error::BadArgument)
    }

    pub fn select(self, id: EditorId, start: u32, end: u32) -> Result<EditorInfo>
    {
        Ok(self.request(TextEditCall { id,
                                       operation: SELECT,
                                       info:
                                           mac_abi::textedit::EditorInfo { selection_start: start,
                                                                           selection_end: end,
                                                                           ..Default::default() },
                                       ..Default::default() },
                        "",
                        &mut [])?
               .info
               .into())
    }

    pub fn key(self, id: EditorId, key: char) -> Result<EditorInfo>
    {
        Ok(self.request(TextEditCall { id,
                                       operation: KEY,
                                       key: key as u32,
                                       ..Default::default() },
                        "",
                        &mut [])?
               .info
               .into())
    }

    /// Native movement with an optional fixed selection anchor. Unlike classic
    /// TEKey, unmodified left/right collapse a selection without an extra step.
    pub fn move_cursor(self, id: EditorId, movement: CursorMovement, extend: bool)
                       -> Result<EditorInfo>
    {
        Ok(self.request(TextEditCall { id,
                                       operation: NAVIGATE,
                                       key: movement as u32,
                                       index: u32::from(extend),
                                       ..Default::default() },
                        "",
                        &mut [])?
               .info
               .into())
    }

    /// Native scrolling keeps wide document offsets separate from classic rects.
    pub fn scroll_to(self, id: EditorId, pixels: u32) -> Result<EditorInfo>
    {
        Ok(self.request(TextEditCall { operation: SCROLL_TO,
                                       id,
                                       index: pixels,
                                       ..Default::default() },
                        "",
                        &mut [])?
               .info
               .into())
    }

    pub fn reveal_selection(self, id: EditorId) -> Result<EditorInfo>
    {
        Ok(self.request(TextEditCall { operation: REVEAL_SELECTION,
                                       id,
                                       ..Default::default() },
                        "",
                        &mut [])?
               .info
               .into())
    }

    pub fn line(self, id: EditorId, index: u32) -> Result<LineInfo>
    {
        Ok(self.request(TextEditCall { id,
                                       operation: LINE,
                                       index,
                                       ..Default::default() },
                        "",
                        &mut [])?
               .line
               .into())
    }

    pub fn offset(self, id: EditorId, point: Point) -> Result<u32>
    {
        Ok(self.request(TextEditCall { id,
                                       operation: OFFSET,
                                       point,
                                       ..Default::default() },
                        "",
                        &mut [])?
               .index)
    }

    pub fn point(self, id: EditorId, index: u32) -> Result<Point>
    {
        Ok(self.request(TextEditCall { id,
                                       operation: POINT,
                                       index,
                                       ..Default::default() },
                        "",
                        &mut [])?
               .point)
    }

    pub fn scroll(self, id: EditorId, dh: i16, dv: i16) -> Result<EditorInfo>
    {
        Ok(self.request(TextEditCall { id,
                                       operation: SCROLL,
                                       delta: Point { h: dh,
                                                      v: dv },
                                       ..Default::default() },
                        "",
                        &mut [])?
               .info
               .into())
    }

    pub fn activate(self, id: EditorId, active: bool) -> Result<EditorInfo>
    {
        Ok(self.request(TextEditCall { id,
                                       operation: ACTIVATE,
                                       index: u32::from(active),
                                       ..Default::default() },
                        "",
                        &mut [])?
               .info
               .into())
    }

    /// Enable the automatic I-beam over this editor's most recently drawn view.
    /// Editors default to enabled; read-only fields can disable it independently
    /// of keyboard activation or selection visibility.
    pub fn set_cursor_enabled(self, id: EditorId, enabled: bool) -> Result<EditorInfo>
    {
        Ok(self.request(TextEditCall { id,
                                       operation: SET_CURSOR_ENABLED,
                                       index: u32::from(enabled),
                                       ..Default::default() },
                        "",
                        &mut [])?
               .info
               .into())
    }

    pub fn idle(self, id: EditorId, ticks: u32) -> Result<EditorInfo>
    {
        Ok(self.request(TextEditCall { id,
                                       operation: IDLE,
                                       ticks,
                                       ..Default::default() },
                        "",
                        &mut [])?
               .info
               .into())
    }

    pub fn click(self, id: EditorId, point: Point, extend: bool) -> Result<EditorInfo>
    {
        Ok(self.request(TextEditCall { id,
                                       operation: CLICK,
                                       point,
                                       index: u32::from(extend),
                                       ..Default::default() },
                        "",
                        &mut [])?
               .info
               .into())
    }

    /// Append a snapshot to the window's retained frame, preserving its port
    /// clip. Call inside BeginUpdate/EndUpdate after erasing the view, and redraw
    /// after edits or caret changes. Drawing text and selection is transparent;
    /// this operation does not erase earlier retained commands.
    pub fn draw(self, id: EditorId, window: WindowId) -> Result<()>
    {
        self.request(TextEditCall { id,
                                    operation: DRAW,
                                    window,
                                    ..Default::default() },
                     "",
                     &mut [])
            .map(|_| ())
    }
}

/// A checked routing reference; this value does not own the editor lifetime.
#[derive(Clone, Copy)]
pub struct EditorRef<'a>
{
    client: Client<'a>,
    id:     EditorId
}
/// The single destruction duty for an editor. Drop makes a best-effort disposal;
/// explicit disposal retains ownership on service errors so callers can retry.
#[must_use = "dropping an editor owner disposes it"]
pub struct Editor<'a>
{
    editor: EditorRef<'a>,
    live:   bool
}
impl<'a> TextEdit<'a>
{
    pub fn editor(self, id: EditorId) -> EditorRef<'a>
    {
        EditorRef { client: self.client,
                    id }
    }

    pub fn create_owned(self, dest: Rect, view: Rect) -> Result<Editor<'a>>
    {
        Ok(Editor { editor: self.editor(self.create(dest, view)?),
                    live:   true })
    }
}
impl<'a> Editor<'a>
{
    pub fn id(&self) -> EditorId
    {
        self.editor.id
    }

    pub fn editor(&self) -> EditorRef<'a>
    {
        self.editor
    }

    pub fn into_id(self) -> EditorId
    {
        let id = self.id();
        core::mem::forget(self);
        id
    }

    pub fn dispose(&mut self) -> Result<()>
    {
        if self.live
        {
            match self.editor.dispose()
            {
                Ok(()) | Err(Error::NotFound) => self.live = false,
                Err(error) => return Err(error)
            }
        }
        Ok(())
    }
}
impl<'a> core::ops::Deref for Editor<'a>
{
    type Target = EditorRef<'a>;

    fn deref(&self) -> &Self::Target
    {
        &self.editor
    }
}
impl Drop for Editor<'_>
{
    fn drop(&mut self)
    {
        let _ = self.dispose();
    }
}

impl EditorRef<'_>
{
    pub fn id(self) -> EditorId
    {
        self.id
    }

    pub fn dispose(self) -> Result<()>
    {
        TextEdit::new(self.client).dispose(self.id)
    }

    pub fn copy(self) -> Result<EditorInfo>
    {
        TextEdit::new(self.client).copy(self.id)
    }

    pub fn cut(self) -> Result<EditorInfo>
    {
        TextEdit::new(self.client).cut(self.id)
    }

    pub fn paste(self) -> Result<EditorInfo>
    {
        TextEdit::new(self.client).paste(self.id)
    }

    pub fn set_rects(self, dest: Rect, view: Rect) -> Result<EditorInfo>
    {
        TextEdit::new(self.client).set_rects(self.id, dest, view)
    }

    pub fn info(self) -> Result<EditorInfo>
    {
        TextEdit::new(self.client).info(self.id)
    }

    pub fn set_font(self, font: FontSelection) -> Result<EditorInfo>
    {
        TextEdit::new(self.client).set_font(self.id, font)
    }

    pub fn set_text(self, text: &str) -> Result<EditorInfo>
    {
        TextEdit::new(self.client).set_text(self.id, text)
    }

    pub fn insert(self, text: &str) -> Result<EditorInfo>
    {
        TextEdit::new(self.client).insert(self.id, text)
    }

    pub fn delete(self) -> Result<EditorInfo>
    {
        TextEdit::new(self.client).delete(self.id)
    }

    pub fn text(self, output: &mut [u8]) -> Result<&str>
    {
        TextEdit::new(self.client).text(self.id, output)
    }

    pub fn selection_text(self, output: &mut [u8]) -> Result<&str>
    {
        TextEdit::new(self.client).selection_text(self.id, output)
    }

    pub fn select(self, start: u32, end: u32) -> Result<EditorInfo>
    {
        TextEdit::new(self.client).select(self.id, start, end)
    }

    pub fn key(self, key: char) -> Result<EditorInfo>
    {
        TextEdit::new(self.client).key(self.id, key)
    }

    pub fn move_cursor(self, movement: CursorMovement, extend: bool) -> Result<EditorInfo>
    {
        TextEdit::new(self.client).move_cursor(self.id, movement, extend)
    }

    pub fn line(self, index: u32) -> Result<LineInfo>
    {
        TextEdit::new(self.client).line(self.id, index)
    }

    pub fn offset(self, point: Point) -> Result<u32>
    {
        TextEdit::new(self.client).offset(self.id, point)
    }

    pub fn point(self, index: u32) -> Result<Point>
    {
        TextEdit::new(self.client).point(self.id, index)
    }

    pub fn scroll(self, dh: i16, dv: i16) -> Result<EditorInfo>
    {
        TextEdit::new(self.client).scroll(self.id, dh, dv)
    }

    pub fn activate(self, active: bool) -> Result<EditorInfo>
    {
        TextEdit::new(self.client).activate(self.id, active)
    }

    pub fn set_cursor_enabled(self, enabled: bool) -> Result<EditorInfo>
    {
        TextEdit::new(self.client).set_cursor_enabled(self.id, enabled)
    }

    pub fn idle(self, ticks: u32) -> Result<EditorInfo>
    {
        TextEdit::new(self.client).idle(self.id, ticks)
    }

    pub fn click(self, point: Point, extend: bool) -> Result<EditorInfo>
    {
        TextEdit::new(self.client).click(self.id, point, extend)
    }

    pub fn draw(self, window: WindowId) -> Result<()>
    {
        TextEdit::new(self.client).draw(self.id, window)
    }
}
