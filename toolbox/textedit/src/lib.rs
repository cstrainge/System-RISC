//! Thin resident TextEdit bindings. Editors require explicit disposal; IDs carry
//! generations, so an old ID cannot accidentally access a replacement editor.
#![no_std]
#![forbid(unsafe_code)]
use mac_abi::{BAD_ARGUMENT, textedit::*};
pub use mac_abi::{Point, Rect, WindowId,
                  textedit::{EditorId, EditorInfo, LineInfo, MAX_BYTES, MAX_EDITORS}};
pub use mac_traps::{Client, Error, Result};
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
               .info)
    }

    pub fn create(self, dest: Rect, view: Rect) -> Result<EditorId>
    {
        Ok(self.request(TextEditCall { operation: CREATE,
                                       info: EditorInfo { dest,
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
                                       info: EditorInfo { dest,
                                                          view,
                                                          ..Default::default() },
                                       ..Default::default() },
                        "",
                        &mut [])?
               .info)
    }

    pub fn info(self, id: EditorId) -> Result<EditorInfo>
    {
        self.simple(id, INFO)
    }

    pub fn set_text(self, id: EditorId, text: &str) -> Result<EditorInfo>
    {
        Ok(self.request(TextEditCall { id,
                                       operation: SET_TEXT,
                                       ..Default::default() },
                        text,
                        &mut [])?
               .info)
    }

    pub fn insert(self, id: EditorId, text: &str) -> Result<EditorInfo>
    {
        Ok(self.request(TextEditCall { id,
                                       operation: INSERT,
                                       ..Default::default() },
                        text,
                        &mut [])?
               .info)
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
        core::str::from_utf8(output.get(..call.transferred).ok_or(Error(BAD_ARGUMENT))?).map_err(|_|Error(BAD_ARGUMENT))
    }

    pub fn select(self, id: EditorId, start: u32, end: u32) -> Result<EditorInfo>
    {
        Ok(self.request(TextEditCall { id,
                                       operation: SELECT,
                                       info: EditorInfo { selection_start: start,
                                                          selection_end: end,
                                                          ..Default::default() },
                                       ..Default::default() },
                        "",
                        &mut [])?
               .info)
    }

    pub fn key(self, id: EditorId, key: char) -> Result<EditorInfo>
    {
        Ok(self.request(TextEditCall { id,
                                       operation: KEY,
                                       key: key as u32,
                                       ..Default::default() },
                        "",
                        &mut [])?
               .info)
    }

    pub fn line(self, id: EditorId, index: u32) -> Result<LineInfo>
    {
        Ok(self.request(TextEditCall { id,
                                       operation: LINE,
                                       index,
                                       ..Default::default() },
                        "",
                        &mut [])?
               .line)
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
               .info)
    }

    pub fn activate(self, id: EditorId, active: bool) -> Result<EditorInfo>
    {
        Ok(self.request(TextEditCall { id,
                                       operation: ACTIVATE,
                                       index: u32::from(active),
                                       ..Default::default() },
                        "",
                        &mut [])?
               .info)
    }

    pub fn idle(self, id: EditorId, ticks: u32) -> Result<EditorInfo>
    {
        Ok(self.request(TextEditCall { id,
                                       operation: IDLE,
                                       ticks,
                                       ..Default::default() },
                        "",
                        &mut [])?
               .info)
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
               .info)
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
