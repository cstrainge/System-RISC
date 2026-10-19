//! Resident monochrome/color cursor state and visibility nesting.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use mac_abi::{quickdraw::*, *};

use super::{Canvas, geometry::sect_rect};
#[derive(Default)]
pub struct Cursors
{
    image:      Option<MonoCursor>,
    colors:     Vec<u32>,
    visibility: i32,
    obscured:   Option<Point>
}
impl Cursors
{
    pub fn request(&mut self, call: &mut CursorCall, colors: &[u32], mouse: Point)
                   -> Result<(), Status>
    {
        match call.operation
        {
            CURSOR_SET =>
            {
                if !colors.is_empty() && colors.len() != 256
                {
                    return Err(BAD_ARGUMENT);
                }
                self.image = Some(call.cursor);
                self.colors = colors.to_vec();
            }
            CURSOR_INIT =>
            {
                *self = Self::default();
            }
            CURSOR_HIDE => self.visibility = self.visibility.saturating_sub(1),
            CURSOR_SHOW => self.visibility = self.visibility.saturating_add(1).min(0),
            CURSOR_OBSCURE => self.obscured = Some(mouse),
            CURSOR_SHIELD =>
            {
                let hotspot = self.image.map_or(Point { h: 1,
                                                        v: 1 },
                                                |c| c.hotspot);
                let cursor = Rect::new(mouse.h.saturating_sub(hotspot.h),
                                       mouse.v.saturating_sub(hotspot.v),
                                       mouse.h.saturating_sub(hotspot.h).saturating_add(16),
                                       mouse.v.saturating_sub(hotspot.v).saturating_add(16));
                let mut shield = call.bounds;
                shield.offset(call.offset.h, call.offset.v);
                let mut overlap = Rect::ZERO;
                if sect_rect(cursor, shield, &mut overlap)
                {
                    self.visibility = self.visibility.saturating_sub(1);
                }
            }
            CURSOR_STATE =>
            {}
            _ => return Err(NOT_SUPPORTED)
        }
        call.visibility = self.visibility;
        Ok(())
    }

    pub fn draw(&mut self, canvas: &mut Canvas<'_>, mouse: Point)
    {
        if self.obscured.is_some_and(|at| at != mouse)
        {
            self.obscured = None;
        }
        if self.visibility < 0 || self.obscured.is_some()
        {
            return;
        }
        let Some(cursor) = self.image
        else
        {
            super::cursor::cursor(canvas, mouse);
            return;
        };
        for row in 0..16
        {
            for column in 0..16
            {
                let bit = 0x8000 >> column;
                let image = cursor.image[row] & bit != 0;
                let mask = cursor.mask[row] & bit != 0;
                let x = i32::from(mouse.h) - i32::from(cursor.hotspot.h) + column;
                let y = i32::from(mouse.v) - i32::from(cursor.hotspot.v) + row as i32;
                if !(i32::from(i16::MIN)..i32::from(i16::MAX)).contains(&x)
                   || !(i32::from(i16::MIN)..i32::from(i16::MAX)).contains(&y)
                {
                    continue;
                }
                let rect = Rect::new(x as i16, y as i16, x as i16 + 1, y as i16 + 1);
                if mask
                {
                    canvas.paint_rect(rect,
                                      if self.colors.len() == 256
                                      {
                                          self.colors[row * 16 + column as usize]
                                      }
                                      else if image
                                      {
                                          0
                                      }
                                      else
                                      {
                                          0xffffff
                                      });
                }
                else if image
                {
                    canvas.invert_rect(rect);
                }
            }
        }
    }
}
