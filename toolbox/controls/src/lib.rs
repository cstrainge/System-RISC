//! Control Manager client bindings; standard CDEF drawing lives in System.
#![no_std]
#![forbid(unsafe_code)]
pub use mac_abi::{Point, Rect, WindowId, controls::ScrollbarState,
                  definitions::ControlDefinitionState as StandardState, quickdraw::FontSelection};
pub use sr_control_reflow_handling::{Constraints, Invalidation, LayoutRect, Measurement, Reflow,
                                     Size, SizePreferences};
#[repr(i16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StandardKind
{
    Button                = 0,
    Checkbox              = 1,
    Radio                 = 2,
    WindowFontButton      = 8,
    WindowFontCheckbox    = 9,
    WindowFontRadio       = 10,
    /// Prefer the typed ProgressBar owner for byte counts and unknown totals.
    ProgressBar           = mac_abi::controls::PROGRESS_BAR_PROCEDURE,
    MonochromeProgressBar = mac_abi::controls::MONOCHROME_PROGRESS_BAR_PROCEDURE,
    /// Prefer the typed StaticText owner for UTF-8 text and justification.
    StaticText            = mac_abi::controls::STATIC_TEXT_PROCEDURE
}
#[repr(i16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part
{
    None     = 0,
    Button   = 10,
    Check    = 11,
    Up       = 20,
    Down     = 21,
    PageUp   = 22,
    PageDown = 23,
    Thumb    = 129,
    Inactive = 254
}
impl Part
{
    pub fn from_code(code: i16) -> Result<Self>
    {
        match code
        {
            0 => Ok(Self::None),
            10 => Ok(Self::Button),
            11 => Ok(Self::Check),
            20 => Ok(Self::Up),
            21 => Ok(Self::Down),
            22 => Ok(Self::PageUp),
            23 => Ok(Self::PageDown),
            129 => Ok(Self::Thumb),
            254 => Ok(Self::Inactive),
            _ => Err(Error::BadArgument)
        }
    }
}
pub use mac_abi::controls::{SCROLL_DOWN, SCROLL_PAGE_DOWN, SCROLL_PAGE_UP, SCROLL_THUMB, SCROLL_UP};
pub use mac_traps::{Client, Error, Result};
pub struct Controls<'a>
{
    client: Client<'a>
}
impl<'a> Controls<'a>
{
    pub const fn new(client: Client<'a>) -> Self
    {
        Self { client }
    }

    /// Draw a complete typed state, including classic pressed-part highlights.
    pub fn draw_scrollbar_state(&self, window: WindowId, scrollbar: ScrollbarState) -> Result<()>
    {
        let mut call = mac_abi::controls::ControlCall { operation:
                                                            mac_abi::controls::DRAW_SCROLLBAR,
                                                        window: window.0,
                                                        scrollbar,
                                                        ..Default::default() };
        self.client.control(&mut call)
    }

    pub fn draw_scrollbar(&self, window: WindowId, bounds: Rect, value: i16, maximum: i16,
                          active: bool)
                          -> Result<()>
    {
        let mut call=mac_abi::controls::ControlCall {operation:mac_abi::controls::DRAW_SCROLLBAR,window:window.0,
            scrollbar:mac_abi::controls::ScrollbarState {bounds,value,maximum,active:u32::from(active),..Default::default()},..Default::default()};
        self.client.control(&mut call)
    }

    /// Resolve the CDEF hit part into the same vocabulary as standard controls.
    pub fn scrollbar_hit(&self, window: WindowId, bounds: Rect, value: i16, maximum: i16,
                         active: bool, point: mac_abi::Point)
                         -> Result<Part>
    {
        let mut call=mac_abi::controls::ControlCall {operation:mac_abi::controls::HIT_SCROLLBAR,window:window.0,
            scrollbar:mac_abi::controls::ScrollbarState {bounds,value,maximum,active:u32::from(active),..Default::default()},point,..Default::default()};
        self.client.control(&mut call)?;
        Part::from_code(call.result as i16).map_err(|_| Error::BadAbi)
    }

    /// Map movement from a mouse-down anchor using CDEF-owned thumb geometry.
    /// Keep initial_value and anchor unchanged for the whole drag.
    pub fn scrollbar_track_value(&self, window: WindowId, bounds: Rect, initial_value: i16,
                                 maximum: i16, anchor: mac_abi::Point, point: mac_abi::Point)
                                 -> Result<i16>
    {
        let mut call=mac_abi::controls::ControlCall {operation:mac_abi::controls::TRACK_SCROLLBAR,window:window.0,
            scrollbar:mac_abi::controls::ScrollbarState {bounds,value:initial_value,maximum,active:1,..Default::default()},anchor,point,..Default::default()};
        self.client.control(&mut call)?;
        Ok(call.result as i16)
    }

    pub fn draw_standard_state(&self, window: WindowId, kind: StandardKind,
                               standard: StandardState)
                               -> Result<()>
    {
        let mut call = mac_abi::controls::ControlCall { operation:
                                                            mac_abi::controls::DRAW_STANDARD,
                                                        window: window.0,
                                                        standard,
                                                        procedure: kind as i16,
                                                        ..Default::default() };
        self.client.control(&mut call)
    }

    pub fn standard_hit(&self, kind: StandardKind, standard: StandardState, point: Point)
                        -> Result<Part>
    {
        let mut call = mac_abi::controls::ControlCall { operation:
                                                            mac_abi::controls::HIT_STANDARD,
                                                        standard,
                                                        procedure: kind as i16,
                                                        point,
                                                        ..Default::default() };
        self.client.control(&mut call)?;
        Part::from_code(call.result as i16)
    }

    /// Draw directly into exclusive caller-owned RGB pixels, using resident CDEF0.
    pub fn draw_standard_pixels(&self, pixels: &mut [u32], width: usize, height: usize,
                                kind: StandardKind, standard: StandardState)
                                -> Result<()>
    {
        let mut call = mac_abi::controls::ControlCall { operation:
                                                            mac_abi::controls::DRAW_STANDARD,
                                                        standard,
                                                        procedure: kind as i16,
                                                        ..Default::default() };
        self.client.control_pixels(&mut call, pixels, width, height)
    }

    /// Draw directly into exclusive caller-owned RGB pixels, using resident CDEF1.
    pub fn draw_scrollbar_pixels(&self, pixels: &mut [u32], width: usize, height: usize,
                                 scrollbar: ScrollbarState)
                                 -> Result<()>
    {
        let mut call = mac_abi::controls::ControlCall { operation:
                                                            mac_abi::controls::DRAW_SCROLLBAR,
                                                        scrollbar,
                                                        ..Default::default() };
        self.client.control_pixels(&mut call, pixels, width, height)
    }

    pub fn button(&self, window: WindowId, bounds: Rect, title: &str) -> Result<()>
    {
        toolbox_quickdraw::QuickDraw::new(self.client).port(window).command(toolbox_quickdraw::DrawOperation::Button,
                                                                            bounds,
                                                                            0,
                                                                            title)
    }
}
pub use mac_abi::definitions::{CDEF, ControlDefinitionState, DEFAULT_BUTTON};

pub mod scrollbox;
pub use scrollbox::{ScrollBox, ScrollBoxId, ScrollBoxRef, ScrollBoxSpec};
mod capture;
pub mod objects;
pub use objects::{Control, ControlId, ControlInfo, ControlRef};
mod passive;
pub use passive::{Progress, ProgressBar, ProgressStyle, StaticText, TextAlignment};
