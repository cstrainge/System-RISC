//! Drawing primitives over the firmware's framebuffer.
//!
//! Every write goes through the returned pitch, so a wrong stride shows up as a
//! skewed fill rather than being hidden by a width-only assumption. The caller
//! cleans the range before the scanout is expected to see it.
use core::ptr::write_volatile;

use mac_videocore_fb::Framebuffer;

use crate::{cache, font, qr::Qr};

/// The firmware's 32-bit scanout words put the red channel in the low byte on
/// this machine, so a colour is stated in display channels and packed once.
const fn colour(red: u32, green: u32, blue: u32) -> u32
{
    red | (green << 8) | (blue << 16)
}

pub const GREY: u32 = colour(0x60, 0x60, 0x60);
pub const WHITE: u32 = colour(0xff, 0xff, 0xff);
pub const BLACK: u32 = colour(0x00, 0x00, 0x00);
pub const GREEN: u32 = colour(0x00, 0xc8, 0x53);
pub const RED: u32 = colour(0xe5, 0x39, 0x35);
pub const YELLOW: u32 = colour(0xff, 0xd5, 0x4f);
pub const CYAN: u32 = colour(0x00, 0xe5, 0xff);

pub struct Screen
{
    base:   usize,
    pitch:  usize,
    width:  usize,
    height: usize
}

impl Screen
{
    pub fn new(framebuffer: &Framebuffer) -> Self
    {
        Self { base:   framebuffer.base as usize,
               pitch:  framebuffer.pitch as usize,
               width:  framebuffer.width as usize,
               height: framebuffer.height as usize }
    }

    pub fn width(&self) -> usize
    {
        self.width
    }

    pub fn height(&self) -> usize
    {
        self.height
    }

    pub fn bytes(&self) -> usize
    {
        self.pitch * self.height
    }

    /// One pixel, clipped to the visible area.
    pub fn pixel(&self, x: usize, y: usize, colour: u32)
    {
        if x >= self.width || y >= self.height
        {
            return;
        }
        let address = self.base + y * self.pitch + x * 4;
        // SAFETY: the firmware allocated at least pitch × height bytes at the
        // masked base, and the bounds check keeps the write inside that area.
        unsafe { write_volatile(address as *mut u32, colour) };
    }

    pub fn fill_rect(&self, x: usize, y: usize, width: usize, height: usize, colour: u32)
    {
        let right = (x + width).min(self.width);
        let bottom = (y + height).min(self.height);
        for row in y..bottom
        {
            for column in x..right
            {
                self.pixel(column, row, colour);
            }
        }
    }

    /// Fill every row of the allocation, not just the visible width. A correct
    /// rectangle here is evidence the returned pitch is the real stride.
    pub fn fill_all(&self, colour: u32)
    {
        for row in 0..self.height
        {
            let address = self.base + row * self.pitch;
            for column in 0..self.pitch / 4
            {
                // SAFETY: the row lies inside the allocation.
                unsafe { write_volatile((address + column * 4) as *mut u32, colour) };
            }
        }
    }

    /// A border inset by `inset` pixels from the visible edges.
    pub fn border(&self, inset: usize, thickness: usize, colour: u32)
    {
        let left = inset;
        let top = inset;
        let right = self.width.saturating_sub(inset + thickness);
        let bottom = self.height.saturating_sub(inset + thickness);
        self.fill_rect(left, top, right + thickness - left, thickness, colour);
        self.fill_rect(left, bottom, right + thickness - left, thickness, colour);
        self.fill_rect(left, top, thickness, bottom + thickness - top, colour);
        self.fill_rect(right, top, thickness, bottom + thickness - top, colour);
    }

    pub fn text(&self, x: usize, y: usize, scale: usize, colour: u32, text: &str)
    {
        let mut at = x;
        for character in text.bytes()
        {
            let glyph = font::glyph(character);
            for (row, bits) in glyph.iter().enumerate()
            {
                for column in 0..font::WIDTH
                {
                    if bits >> (font::WIDTH - 1 - column) & 1 != 0
                    {
                        self.fill_rect(at + column * scale, y + row * scale, scale, scale, colour);
                    }
                }
            }
            at += (font::WIDTH + 1) * scale;
        }
    }

    pub fn text_width(scale: usize, text: &str) -> usize
    {
        text.len() * (font::WIDTH + 1) * scale
    }

    pub fn centred_text(&self, y: usize, scale: usize, colour: u32, text: &str)
    {
        let width = Self::text_width(scale, text);
        let x = self.width.saturating_sub(width) / 2;
        self.text(x, y, scale, colour, text);
    }

    /// A QR symbol on its own white panel, with the quiet zone part of the
    /// panel. `scale` is pixels per module and must divide nothing: it is used
    /// as an integer count.
    pub fn qr(&self, x: usize, y: usize, scale: usize, qr: &Qr, foreground: u32, background: u32)
    {
        let modules = qr.size;
        let panel = (modules + 8) * scale;
        self.fill_rect(x, y, panel, panel, background);
        for row in 0..modules
        {
            for column in 0..modules
            {
                if qr.dark(column, row)
                {
                    self.fill_rect(x + (4 + column) * scale,
                                   y + (4 + row) * scale,
                                   scale,
                                   scale,
                                   foreground);
                }
            }
        }
    }

    /// Write the QR modules only, with no panel and no clean. Used by the
    /// cache demonstration, where the point is that the scanout does not see
    /// them.
    pub fn qr_modules_only(&self, x: usize, y: usize, scale: usize, qr: &Qr, colour: u32)
    {
        for row in 0..qr.size
        {
            for column in 0..qr.size
            {
                if qr.dark(column, row)
                {
                    self.fill_rect(x + (4 + column) * scale,
                                   y + (4 + row) * scale,
                                   scale,
                                   scale,
                                   colour);
                }
            }
        }
    }

    /// The largest integer module scale whose panel fits in `available` pixels.
    pub fn qr_scale(qr: &Qr, available: usize) -> usize
    {
        (available / (qr.size + 8)).max(1)
    }

    /// Clean the whole scanout to the point of coherency. Until this returns,
    /// the display is entitled to show the previous contents.
    pub fn flush(&self)
    {
        cache::clean(self.base, self.bytes());
    }
}
