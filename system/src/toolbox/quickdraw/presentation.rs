//! Present-time cursor compositing. The retained scene is always read-only.
#![forbid(unsafe_code)]
use mac_abi::{Point, Rect, quickdraw::MonoCursor};

#[derive(Clone, Copy)]
pub(crate) struct CursorSprite<'a>
{
    image:  MonoCursor,
    colors: &'a [u32],
    left:   i32,
    top:    i32
}
impl<'a> CursorSprite<'a>
{
    pub(super) fn new(image: MonoCursor, colors: &'a [u32], mouse: Point) -> Self
    {
        Self { image,
               colors,
               left: i32::from(mouse.h) - i32::from(image.hotspot.h),
               top: i32::from(mouse.v) - i32::from(image.hotspot.v) }
    }

    pub(crate) fn bounds(self) -> Rect
    {
        let coordinate = |value: i32| value.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        Rect::new(coordinate(self.left),
                  coordinate(self.top),
                  coordinate(self.left + 16),
                  coordinate(self.top + 16))
    }

    fn composite_row(self, pixels: &mut [u32], left: i32, y: i32)
    {
        let row = y - self.top;
        if !(0..16).contains(&row)
        {
            return;
        }
        let start = self.left.max(left);
        let end = (self.left + 16).min(left + pixels.len() as i32);
        for x in start..end
        {
            let column = (x - self.left) as usize;
            let row = row as usize;
            let pixel = &mut pixels[(x - left) as usize];
            let bit = 0x8000 >> column;
            let image = self.image.image[row] & bit != 0;
            let mask = self.image.mask[row] & bit != 0;
            if self.colors.len() == 256
            {
                let color = self.colors[row * 16 + column] & 0xffffff;
                if mask
                {
                    *pixel = color;
                }
                else
                {
                    *pixel ^= !color & 0xffffff;
                }
            }
            else if mask
            {
                *pixel = if image { 0 } else { 0xffffff };
            }
            else if image
            {
                *pixel ^= 0xffffff;
            }
        }
    }
}

/// Copy one clipped damage rectangle and composite only its cursor pixels.
/// Refuse invalid surface extents before writing any frontbuffer pixels.
pub(crate) fn present_rect(scene: &[u32], front: &mut [u32], width: usize, height: usize,
                           rect: Rect, cursor: Option<CursorSprite<'_>>)
                           -> Option<Rect>
{
    let length = width.checked_mul(height)?;
    if width > i16::MAX as usize
       || height > i16::MAX as usize
       || scene.len() < length
       || front.len() < length
    {
        return None;
    }
    let rect = Rect::new(rect.left.clamp(0, width as i16),
                         rect.top.clamp(0, height as i16),
                         rect.right.clamp(0, width as i16),
                         rect.bottom.clamp(0, height as i16));
    if rect.is_empty()
    {
        return None;
    }
    let left = rect.left as usize;
    let right = rect.right as usize;
    for y in rect.top as usize..rect.bottom as usize
    {
        let at = y * width;
        let output = &mut front[at + left..at + right];
        output.copy_from_slice(&scene[at + left..at + right]);
    }
    if let Some(cursor) = cursor
    {
        let mut overlap = Rect::ZERO;
        if super::geometry::sect_rect(rect, cursor.bounds(), &mut overlap)
        {
            for y in overlap.top as usize..overlap.bottom as usize
            {
                let at = y * width;
                cursor.composite_row(&mut front
                                         [at + overlap.left as usize..at + overlap.right as usize],
                                     i32::from(overlap.left),
                                     y as i32);
            }
        }
    }
    Some(rect)
}
