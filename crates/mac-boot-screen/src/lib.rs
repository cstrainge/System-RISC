//! Shared boot rasters and desktop pattern. No allocation or Toolbox runtime.
#![no_std]
#![deny(unsafe_code)]
use mac_abi::Rect;
#[cfg(target_arch = "riscv64")]
#[allow(unsafe_code)]
pub mod ramfb;
pub const HAPPY: &[u8; 256] = include_bytes!("../assets/happy-mac.icn");
pub const MISSING: &[u8; 256] = include_bytes!("../assets/missing-disk.icn");
pub const BLANK_DISK: &[u8; 256] = include_bytes!("../assets/blank-disk.icn");

pub struct Raster<'a>
{
    pixels: &'a mut [u32],
    width:  usize,
    height: usize
}
impl<'a> Raster<'a>
{
    pub fn new(pixels: &'a mut [u32], width: usize, height: usize) -> Option<Self>
    {
        if width == 0
           || height == 0
           || width > i16::MAX as usize
           || height > i16::MAX as usize
           || width.checked_mul(height)? != pixels.len()
        {
            return None;
        }
        Some(Self { pixels,
                    width,
                    height })
    }

    pub fn fill(&mut self, r: Rect, color: u32)
    {
        let left = (r.left.max(0) as usize).min(self.width);
        let right = (r.right.max(0) as usize).min(self.width);
        let top = (r.top.max(0) as usize).min(self.height);
        let bottom = (r.bottom.max(0) as usize).min(self.height);
        if left >= right
        {
            return;
        }
        for y in top..bottom
        {
            self.pixels[y * self.width + left..y * self.width + right].fill(color);
        }
    }

    pub fn background(&mut self)
    {
        self.background_rect(Rect::new(0, 0, self.width as i16, self.height as i16));
    }

    /// Restore just the changed desktop area, preserving global pattern phase.
    pub fn background_rect(&mut self, rect: Rect)
    {
        let left = (rect.left.max(0) as usize).min(self.width);
        let right = (rect.right.max(0) as usize).min(self.width);
        let top = (rect.top.max(0) as usize).min(self.height);
        let bottom = (rect.bottom.max(0) as usize).min(self.height);
        if left >= right || top >= bottom
        {
            return;
        }
        for y in top..bottom.min(top + 2)
        {
            for x in left..right
            {
                self.pixels[y * self.width + x] =
                    if (x + y) & 1 == 0 { 0x878787 } else { 0xc0c0c0 };
            }
        }
        for y in top + 2..bottom
        {
            let start = (top + ((y - top) & 1)) * self.width + left;
            self.pixels.copy_within(start..start + right - left, y * self.width + left);
        }
    }

    pub fn icon(&mut self, bytes: &[u8; 256], left: i16, top: i16, scale: u8)
    {
        if scale == 0
        {
            return;
        }
        for y in 0..32
        {
            for x in 0..32
            {
                let bit = 0x80 >> (x & 7);
                let at = y * 4 + x / 8;
                if bytes[128 + at] & bit == 0
                {
                    continue;
                }
                let color = if bytes[at] & bit == 0 { 0xffffff } else { 0 };
                let left = i32::from(left) + (x as i32) * i32::from(scale);
                let top = i32::from(top) + (y as i32) * i32::from(scale);
                let r = Rect::new(left.clamp(0, i32::from(i16::MAX)) as i16,
                                  top.clamp(0, i32::from(i16::MAX)) as i16,
                                  (left + i32::from(scale)).clamp(0, i32::from(i16::MAX)) as i16,
                                  (top + i32::from(scale)).clamp(0, i32::from(i16::MAX)) as i16);
                self.fill(r, color);
            }
        }
    }
}
/// Welcome geometry preserves the original 300x176 panel and 4px shadow;
/// placement is centered on the actual framebuffer rather than a fixed mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelLayout
{
    pub bounds:         Rect,
    pub progress:       Rect,
    pub title_baseline: i16
}
impl PanelLayout
{
    pub fn new(width: i16, height: i16) -> Self
    {
        let left = (width - 300) / 2;
        let top = (height - 180) / 2;
        Self { bounds:         Rect::new(left, top, left + 300, top + 176),
               progress:       Rect::new(left + 34, top + 139, left + 266, top + 155),
               title_baseline: top + 121 }
    }
}
/// Draw only the welcome panel. Progress is a resource-backed managed control
/// owned by System; the ROM never carries a duplicate progress implementation.
pub fn draw_panel(raster: &mut Raster<'_>, happy: &[u8; 256])
{
    let panel = PanelLayout::new(raster.width as i16, raster.height as i16).bounds;
    raster.background();
    raster.fill(Rect::new(panel.left + 4, panel.top + 4, panel.right + 4, panel.bottom + 4), 0);
    raster.fill(panel, 0);
    raster.fill(Rect::new(panel.left + 2, panel.top + 2, panel.right - 2, panel.bottom - 2),
                0xffffff);
    raster.icon(happy, panel.left + 118, panel.top + 20, 2);
}
