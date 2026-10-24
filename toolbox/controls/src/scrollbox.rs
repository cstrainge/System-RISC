//! Owned, resident virtual row viewports. Applications supply newly exposed rows;
//! System retains pixels, validates identities and tracks the CDEF scrollbar.
pub use mac_abi::scrollbox::Spec as ScrollBoxSpec;
use mac_abi::scrollbox::*;

use crate::{Client, Controls, Error, Point, Result, WindowId};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrollBoxId(pub u64);
#[derive(Clone, Debug)]
pub struct View
{
    pub spec:     ScrollBoxSpec,
    pub top:      u32,
    pub missing:  core::ops::Range<u32>,
    pub tracking: bool,
    pub changed:  bool
}
impl View
{
    pub fn rows(&self) -> u32
    {
        ((self.spec.viewport.bottom - self.spec.viewport.top) / self.spec.row_height) as u32
    }

    pub fn maximum(&self) -> u32
    {
        self.spec.count.saturating_sub(self.rows())
    }
}
fn view(call: Call) -> View
{
    View { spec:     call.spec,
           top:      call.top,
           missing:  call.first..call.end,
           tracking: call.flags & TRACKING != 0,
           changed:  call.flags & CHANGED != 0 }
}
#[derive(Clone, Copy)]
pub struct ScrollBoxRef<'a>
{
    client: Client<'a>,
    id:     ScrollBoxId
}
pub struct ScrollBox<'a>
{
    control: ScrollBoxRef<'a>
}
impl<'a> Controls<'a>
{
    /// Window ID zero creates an offscreen viewport for draw_pixels.
    pub fn create_scroll_box(&self, window: WindowId, spec: ScrollBoxSpec)
                             -> Result<ScrollBox<'a>>
    {
        let mut call = Call { operation: CREATE,
                              window: window.0,
                              spec,
                              ..Default::default() };
        self.client.scroll_box(&mut call)?;
        Ok(ScrollBox { control: self.scroll_box(ScrollBoxId(call.id)) })
    }

    pub fn scroll_box(&self, id: ScrollBoxId) -> ScrollBoxRef<'a>
    {
        ScrollBoxRef { client: self.client,
                       id }
    }
}
impl<'a> ScrollBox<'a>
{
    pub fn id(&self) -> ScrollBoxId
    {
        self.control.id
    }

    pub fn control(&self) -> ScrollBoxRef<'a>
    {
        self.control
    }
}
impl Drop for ScrollBox<'_>
{
    fn drop(&mut self)
    {
        let _ = self.control.dispose();
    }
}
impl ScrollBoxRef<'_>
{
    fn call(self, mut call: Call) -> Result<View>
    {
        call.id = self.id.0;
        self.client.scroll_box(&mut call)?;
        Ok(view(call))
    }

    pub fn info(self) -> Result<View>
    {
        self.call(Call::default())
    }

    /// Hit-test resident row geometry. Blank space, partial rows and the bar
    /// return None; stale IDs and foreign ownership remain service errors.
    pub fn row_at(self, point: crate::Point) -> Result<Option<u32>>
    {
        let mut call = Call { operation: ROW_AT,
                              id: self.id.0,
                              point,
                              ..Default::default() };
        self.client.scroll_box(&mut call)?;
        Ok((call.row != u32::MAX).then_some(call.row))
    }

    pub fn configure(self, spec: ScrollBoxSpec) -> Result<View>
    {
        self.call(Call { operation: CONFIGURE,
                         spec,
                         ..Default::default() })
    }

    pub fn invalidate(self) -> Result<View>
    {
        self.call(Call { operation: INVALIDATE,
                         ..Default::default() })
    }

    pub fn scroll_to(self, top: u32) -> Result<View>
    {
        self.call(Call { operation: SCROLL_TO,
                         top,
                         ..Default::default() })
    }

    pub fn set_row(self, index: u32, text: &str, selected: bool) -> Result<()>
    {
        if text.len() > TEXT_BYTES
        {
            return Err(Error::BadArgument);
        }
        let mut call = Call { operation: SET_ROW,
                              row: index,
                              flags: if selected { SELECTED } else { 0 },
                              text_length: text.len() as u32,
                              ..Default::default() };
        call.text[..text.len()].copy_from_slice(text.as_bytes());
        self.call(call).map(|_| ())
    }

    pub fn draw(self) -> Result<()>
    {
        self.call(Call { operation: DRAW,
                         ..Default::default() })
            .map(|_| ())
    }

    pub fn dispose(self) -> Result<()>
    {
        self.call(Call { operation: DISPOSE,
                         ..Default::default() })
            .map(|_| ())
    }

    pub fn draw_pixels(self, pixels: &mut [u32], width: usize, height: usize) -> Result<()>
    {
        let mut call = Call { operation: DRAW,
                              id: self.id.0,
                              ..Default::default() };
        self.client.scroll_box_pixels(&mut call, pixels, width, height)
    }

    /// The thin event-pump bridge never decides geometry, repeat timing or row
    /// ranges. Callback errors leave the previously committed raster visible.
    pub fn track(self, anchor: Point, mut paint: impl FnMut(View) -> Result<()>) -> Result<()>
    {
        let mut input = mac_abi::events::EventCall { operation: mac_abi::events::INPUT_STATE,
                                                     ..Default::default() };
        self.client.event(&mut input)?;
        let mut state = self.call(Call { operation: TRACK_BEGIN,
                                         point: anchor,
                                         ticks: input.uptime_ticks,
                                         ..Default::default() })?;
        if !state.tracking
        {
            return Ok(());
        }
        let mut query = Call { id: self.id.0,
                               ..Default::default() };
        self.client.scroll_box(&mut query)?;
        let origin = self.client
                         .window(mac_abi::GET_WINDOW,
                                 mac_abi::WindowRequest { id:             query.window,
                                                          bounds:         crate::Rect::ZERO,
                                                          title:
                                                              [0; mac_abi::WINDOW_TITLE_BYTES],
                                                          title_length:   0,
                                                          procedure:      0,
                                                          minimum_width:  1,
                                                          minimum_height: 1,
                                                          reserved:       0 })?
                         .bounds;
        loop
        {
            if !state.missing.is_empty()
            {
                paint(state.clone())?;
            }
            if state.changed || !state.missing.is_empty()
            {
                self.draw()?;
            }
            if !state.tracking
            {
                return Ok(());
            }
            self.client.wait_event(0, 1)?;
            self.client.event(&mut input)?;
            state = self.call(Call { operation: TRACK_POLL,
                                     point: Point { h: input.event.where_.h - origin.left,
                                                    v: input.event.where_.v - origin.top },
                                     ticks: input.uptime_ticks,
                                     flags: if input.event.modifiers & mac_abi::events::BTN_STATE
                                               == 0
                                     {
                                         BUTTON_DOWN
                                     }
                                     else
                                     {
                                         0
                                     },
                                     ..Default::default() })?;
        }
    }
}
