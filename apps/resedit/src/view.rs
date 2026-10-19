//! Pane geometry and full-range row positions; CDEF owns scrollbar geometry.
#![forbid(unsafe_code)]
use mac_toolbox::{Point, Rect,
                  controls::{self, ScrollbarState}};
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
#[derive(Clone, Copy)]
pub struct Scroll
{
    pub top:   u32,
    pub rows:  u32,
    pub count: u32
}
impl Scroll
{
    pub fn new(top: u32, rows: u32, count: u32) -> Self
    {
        let mut s = Self { top,
                           rows,
                           count };
        s.top = s.top.min(s.maximum());
        s
    }

    pub fn maximum(self) -> u32
    {
        self.count.saturating_sub(self.rows)
    }

    pub fn state(self, bounds: Rect) -> ScrollbarState
    {
        let maximum = self.maximum().min(i16::MAX as u32) as i16;
        ScrollbarState { bounds,
                         maximum,
                         value: if self.maximum() == 0
                         {
                             0
                         }
                         else
                         {
                             (u64::from(self.top) * maximum as u64 / u64::from(self.maximum()))
                             as i16
                         },
                         active: u32::from(maximum != 0),
                         ..Default::default() }
    }

    pub fn row_for_value(self, value: i16) -> u32
    {
        let maximum = self.maximum().min(i16::MAX as u32);
        if maximum == 0
        {
            0
        }
        else
        {
            ((value.max(0) as u64 * u64::from(self.maximum()) + u64::from(maximum) / 2)
             / u64::from(maximum)).min(u64::from(self.maximum())) as u32
        }
    }

    pub fn step(self, part: i16) -> u32
    {
        let delta = match part
        {
            controls::SCROLL_UP => -1,
            controls::SCROLL_DOWN => 1,
            controls::SCROLL_PAGE_UP => -i64::from(self.rows),
            controls::SCROLL_PAGE_DOWN => i64::from(self.rows),
            _ => 0
        };
        (i64::from(self.top) + delta).clamp(0, i64::from(self.maximum())) as u32
    }
}
