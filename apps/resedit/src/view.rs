//! Pane geometry and full-range row positions; CDEF owns scrollbar geometry.
#![forbid(unsafe_code)]
use mac_toolbox::{Point, Rect};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane
{
    Types,
    Resources,
    Preview
}
#[derive(Clone, Copy)]
pub struct Layout
{
    pub width:  i16,
    pub height: i16
}
impl Layout
{
    pub fn bounds(self, pane: Pane) -> Rect
    {
        let bottom = self.height - 34;
        match pane
        {
            Pane::Types => Rect::new(4, 24, 109, bottom),
            Pane::Resources => Rect::new(113, 24, 272, bottom),
            Pane::Preview => Rect::new(279, 81, self.width - 16, bottom)
        }
    }

    pub fn scrollbar(self, pane: Pane) -> Rect
    {
        let r = self.bounds(pane);
        Rect::new(r.right - 16, r.top, r.right, r.bottom)
    }

    pub fn body(self, pane: Pane) -> Rect
    {
        let r = self.bounds(pane);
        Rect::new(r.left + 1, r.top + 1, r.right - 16, r.bottom - 1)
    }

    pub fn row_height(self, pane: Pane) -> i16
    {
        if pane == Pane::Preview { 14 } else { 16 }
    }

    pub fn rows(self, pane: Pane) -> u32
    {
        let r = self.body(pane);
        ((r.bottom - r.top) / self.row_height(pane)).max(1) as u32
    }

    pub fn baseline(self, pane: Pane, row: u32) -> i16
    {
        self.bounds(pane).top
        + if pane == Pane::Preview { 12 } else { 13 }
        + row as i16 * self.row_height(pane)
    }

    pub fn row_at(self, pane: Pane, point: Point) -> Option<u32>
    {
        let r = self.body(pane);
        r.contains(point)
         .then(|| ((point.v - r.top) / self.row_height(pane)) as u32)
         .filter(|row| *row < self.rows(pane))
    }
}
