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
        for y in 0..self.height.min(2)
        {
            for x in 0..self.width
            {
                self.pixels[y * self.width + x] =
                    if (x + y) & 1 == 0 { 0x878787 } else { 0xc0c0c0 };
            }
        }
        for y in 2..self.height
        {
            let start = (y & 1) * self.width;
            self.pixels.copy_within(start..start + self.width, y * self.width);
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
pub const PANEL: Rect = Rect::new(170, 150, 470, 326);
pub const PROGRESS: Rect = Rect::new(204, 289, 436, 305);
pub const TITLE_BASELINE: i16 = 271;
pub struct Panel
{
    filled: usize,
    total:  u8
}
impl Panel
{
    pub fn begin(raster: &mut Raster<'_>, total: u8, happy: &[u8; 256]) -> Self
    {
        raster.background();
        raster.fill(Rect::new(PANEL.left + 4, PANEL.top + 4, PANEL.right + 4, PANEL.bottom + 4), 0);
        raster.fill(PANEL, 0);
        raster.fill(Rect::new(PANEL.left + 2, PANEL.top + 2, PANEL.right - 2, PANEL.bottom - 2),
                    0xffffff);
        raster.icon(happy, 288, 170, 2);
        raster.fill(PROGRESS, 0);
        raster.fill(Rect::new(PROGRESS.left + 2,
                              PROGRESS.top + 2,
                              PROGRESS.right - 2,
                              PROGRESS.bottom - 2),
                    0xffffff);
        Self { filled: 0,
               total:  total.max(1) }
    }

    /// Only newly completed progress pixels change. Repeated/regressing stages
    /// do no drawing and produce no framebuffer damage.
    pub fn advance(&mut self, raster: &mut Raster<'_>, completed: u8) -> Option<Rect>
    {
        let width = (PROGRESS.right - PROGRESS.left - 4) as usize;
        let next = width * usize::from(completed.min(self.total)) / usize::from(self.total);
        if next <= self.filled
        {
            return None;
        }
        let damage = Rect::new(PROGRESS.left + 2 + self.filled as i16,
                               PROGRESS.top + 2,
                               PROGRESS.left + 2 + next as i16,
                               PROGRESS.bottom - 2);
        for y in damage.top..damage.bottom
        {
            for x in damage.left..damage.right
            {
                raster.fill(Rect::new(x, y, x + 1, y + 1),
                            if (x + y) & 7 < 3 { 0 } else { 0xffffff });
            }
        }
        self.filled = next;
        Some(damage)
    }
}
