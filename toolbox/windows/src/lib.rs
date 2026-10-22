//! Idiomatic Window Manager client. State, drawing and tracking live in System.
#![no_std]
#![forbid(unsafe_code)]
use mac_abi::*;
pub use mac_abi::{Point, Rect, WindowId};
use mac_traps::types::InlineText;
pub use mac_traps::{Client, Error, Result};

/// An immutable native snapshot; transport buffers and request fields stay private.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowInfo
{
    pub id:        WindowId,
    pub bounds:    Rect,
    pub title:     InlineText<WINDOW_TITLE_BYTES>,
    pub procedure: i16,
    pub minimum:   (i16, i16),
    pub zoomed:    bool
}
impl WindowInfo
{
    pub fn title(&self) -> &str
    {
        self.title.as_str()
    }
}
impl TryFrom<WindowRequest> for WindowInfo
{
    type Error = Error;

    fn try_from(request: WindowRequest) -> Result<Self>
    {
        Ok(Self { id:        WindowId(request.id),
                  bounds:    request.bounds,
                  title:     InlineText::from_reply(request.title, request.title_length)?,
                  procedure: request.procedure,
                  minimum:   (request.minimum_width, request.minimum_height),
                  zoomed:    request.reserved != 0 })
    }
}

pub struct WindowSpec<'a>
{
    bounds:    Rect,
    title:     &'a str,
    minimum:   (i16, i16),
    procedure: i16
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
            return Err(Error::BadArgument);
        }
        request.bounds = spec.bounds;
        request.title[..length].copy_from_slice(&spec.title.as_bytes()[..length]);
        request.title_length = length as u32;
        request.minimum_width = spec.minimum.0;
        request.minimum_height = spec.minimum.1;
        request.procedure = spec.procedure;
        Ok(WindowId(self.client.window(NEW_WINDOW, request)?.id))
    }

    /// Create a uniquely owned window which closes when its Rust owner drops.
    /// The existing ID-based constructor remains available for explicit lifecycles.
    pub fn create_owned(self, spec: WindowSpec<'_>) -> Result<Window<'a>>
    {
        Ok(Window { client: self.client,
                    id:     self.create(spec)?,
                    owned:  true })
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
            return Err(Error::BadArgument);
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

    pub fn info(self, id: WindowId) -> Result<WindowInfo>
    {
        self.client.window(GET_WINDOW, request(id))?.try_into()
    }

    pub fn front(self) -> Result<Option<WindowId>>
    {
        match self.info(WindowId(0))
        {
            Ok(r) => Ok(Some(r.id)),
            Err(Error::NotFound) => Ok(None),
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

    /// Acknowledge queued update damage without changing retained pixels. Useful
    /// while preparing an asynchronous replacement snapshot: commit its exact
    /// changed bounds when ready. Repeated acknowledgement is a no-op; an active
    /// drawing transaction must be completed before acknowledging damage.
    pub fn validate(self, id: WindowId) -> Result<()>
    {
        self.client.window(VALIDATE_WINDOW, request(id)).map(|_| ())
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

    /// Set this window's content-fitting standard zoom state. System owns zoom
    /// button tracking, cancellation and restoration of the user's rectangle.
    /// Updating the proposal does not resize, invalidate or repaint the window.
    pub fn set_standard_bounds(self, id: WindowId, bounds: Option<Rect>) -> Result<()>
    {
        let mut r = request(id);
        r.bounds = bounds.unwrap_or(Rect::ZERO);
        r.reserved = i16::from(bounds.is_none());
        self.client.window(SET_STANDARD_BOUNDS, r).map(|_| ())
    }
}

/// Owns one resident window. Copying its event identity does not copy ownership.
///
/// Drop makes one non-panicking close attempt. Explicit `close` preserves this
/// owner on a service error so callers can retry. As with memory handles, a
/// temporarily busy destructor currently relies on application-exit reclamation;
/// general concurrent cleanup still needs resident deferred disposal.
#[must_use = "dropping the window owner closes its window"]
pub struct Window<'a>
{
    client: Client<'a>,
    id:     WindowId,
    owned:  bool
}
impl Window<'_>
{
    /// Stable identity for event matching and other Toolbox services. This is
    /// non-owning and remains the same after closure; stale requests are checked
    /// by System. It never becomes ID zero, which means the front window.
    pub fn id(&self) -> WindowId
    {
        self.id
    }

    fn windows(&self) -> Windows<'_>
    {
        Windows::new(self.client)
    }

    pub fn info(&self) -> Result<WindowInfo>
    {
        self.windows().info(self.id)
    }

    pub fn set_title(&self, title: &str) -> Result<()>
    {
        self.windows().set_title(self.id, title)
    }

    pub fn select(&self) -> Result<()>
    {
        self.windows().select(self.id)
    }

    pub fn invalidate(&self, bounds: Rect) -> Result<()>
    {
        self.windows().invalidate(self.id, bounds)
    }

    pub fn set_close_deferred(&self, deferred: bool) -> Result<()>
    {
        self.windows().set_close_deferred(self.id, deferred)
    }

    pub fn set_standard_bounds(&self, bounds: Option<Rect>) -> Result<()>
    {
        self.windows().set_standard_bounds(self.id, bounds)
    }

    /// Close immediately, retaining ownership on failure. An already closed
    /// window (including one closed by its close box) is harmless.
    pub fn close(&mut self) -> Result<()>
    {
        if self.owned
        {
            match self.windows().close(self.id)
            {
                Ok(()) | Err(Error::NotFound) => self.owned = false,
                Err(error) => return Err(error)
            }
        }
        Ok(())
    }

    /// Transfer cleanup responsibility to the caller without closing the window.
    /// Ordinary event matching should use `id`, which retains this Rust owner.
    pub fn into_id(mut self) -> WindowId
    {
        self.owned = false;
        self.id
    }
}
impl Drop for Window<'_>
{
    fn drop(&mut self)
    {
        let _ = self.close();
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
