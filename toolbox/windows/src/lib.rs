//! Idiomatic Window Manager client. State, drawing and tracking live in System.
#![no_std]
#![forbid(unsafe_code)]
use mac_abi::*;
pub use mac_abi::{Point, Rect, WINDOW_CLOSE_REQUEST, WindowId, WindowRequest};
pub use mac_traps::{Client, Error, Result};

pub struct WindowSpec<'a>
{
    pub bounds:    Rect,
    pub title:     &'a str,
    pub minimum:   (i16, i16),
    pub procedure: i16
}
impl<'a> WindowSpec<'a>
{
    pub fn new(bounds: Rect, title: &'a str) -> Self
    {
        Self { bounds,
               title,
               minimum: (1, 1),
               procedure: 0 }
    }

    pub fn minimum(mut self, width: i16, height: i16) -> Self
    {
        self.minimum = (width, height);
        self
    }

    pub fn procedure(mut self, procedure: i16) -> Self
    {
        self.procedure = procedure;
        self
    }
}
#[derive(Clone, Copy)]
pub struct Windows<'a>
{
    client: Client<'a>
}
impl<'a> Windows<'a>
{
    pub const fn new(client: Client<'a>) -> Self
    {
        Self { client }
    }

    pub fn create(self, spec: WindowSpec<'_>) -> Result<WindowId>
    {
        let mut request = request(WindowId(0));
        let length = spec.title.len();
        if length > request.title.len()
        {
            return Err(Error(BAD_ARGUMENT));
        }
        request.bounds = spec.bounds;
        request.title[..length].copy_from_slice(&spec.title.as_bytes()[..length]);
        request.title_length = length as u32;
        request.minimum_width = spec.minimum.0;
        request.minimum_height = spec.minimum.1;
        request.procedure = spec.procedure;
        Ok(WindowId(self.client.window(NEW_WINDOW, request)?.id))
    }

    pub fn close(self, id: WindowId) -> Result<()>
    {
        self.client.window(CLOSE_WINDOW, request(id)).map(|_| ())
    }

    pub fn set_title(self, id: WindowId, title: &str) -> Result<()>
    {
        let mut r = request(id);
        if title.len() > r.title.len()
        {
            return Err(Error(BAD_ARGUMENT));
        }
        r.title[..title.len()].copy_from_slice(title.as_bytes());
        r.title_length = title.len() as u32;
        self.client.window(SET_WINDOW_TITLE, r).map(|_| ())
    }

    /// Let the application save or reject a close before disposing its window.
    /// Accepted close-box tracking produces an OsEvent with WINDOW_CLOSE_REQUEST.
    pub fn set_close_deferred(self, id: WindowId, deferred: bool) -> Result<()>
    {
        let mut r = request(id);
        r.reserved = i16::from(deferred);
        self.client.window(SET_CLOSE_DEFERRED, r).map(|_| ())
    }

    pub fn select(self, id: WindowId) -> Result<()>
    {
        self.client.window(SELECT_WINDOW, request(id)).map(|_| ())
    }

    /// Focus the desktop without closing or reordering any windows.
    pub fn select_desktop(self) -> Result<()>
    {
        self.client.window(SELECT_DESKTOP, request(WindowId(0))).map(|_| ())
    }

    pub fn info(self, id: WindowId) -> Result<WindowRequest>
    {
        self.client.window(GET_WINDOW, request(id))
    }

    pub fn front(self) -> Result<Option<WindowId>>
    {
        match self.info(WindowId(0))
        {
            Ok(r) => Ok(Some(WindowId(r.id))),
            Err(Error(NOT_FOUND)) => Ok(None),
            Err(e) => Err(e)
        }
    }

    pub fn begin_update(self, id: WindowId) -> Result<Rect>
    {
        Ok(self.client.window(BEGIN_UPDATE, request(id))?.bounds)
    }

    pub fn end_update(self, id: WindowId) -> Result<()>
    {
        self.client.window(END_UPDATE, request(id)).map(|_| ())
    }

    pub fn invalidate(self, id: WindowId, bounds: Rect) -> Result<()>
    {
        let mut r = request(id);
        r.bounds = bounds;
        self.client.window(INVALIDATE_WINDOW, r).map(|_| ())
    }

    /// Clip the WDEF's grow image/separator drawing in local coordinates.
    /// Applications with their own scrollbars can expose just the size box.
    /// None restores unrestricted drawing; an empty rectangle hides it.
    pub fn set_grow_icon_clip(self, id: WindowId, clip: Option<Rect>) -> Result<()>
    {
        let mut r = request(id);
        r.bounds = clip.unwrap_or(Rect::ZERO);
        r.reserved = i16::from(clip.is_none());
        self.client.window(SET_GROW_ICON_CLIP, r).map(|_| ())
    }
}
fn request(id: WindowId) -> WindowRequest
{
    WindowRequest { id:             id.0,
                    bounds:         Rect::ZERO,
                    title:          [0; WINDOW_TITLE_BYTES],
                    title_length:   0,
                    procedure:      0,
                    minimum_width:  1,
                    minimum_height: 1,
                    reserved:       0 }
}
pub mod definition
{
    pub use mac_abi::definitions::*;
}
