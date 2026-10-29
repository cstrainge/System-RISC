//! Shared resident capture transitions. CDEFs retain all hit/thumb geometry.
use mac_abi::{DefinitionEntry, Point, Status, TrapGate, controls::*};

#[derive(Clone, Copy)]
pub(super) struct Tracking
{
    pub part: i16,
    anchor:   Point,
    initial:  i16,
    next:     u64,
    point:    Point,
    hot:      bool
}
pub(super) enum Action
{
    None,
    Activate,
    Step(i16),
    Value(i16)
}
pub(super) struct Transition
{
    pub tracking:  Option<Tracking>,
    pub highlight: i16,
    pub action:    Action
}
impl Tracking
{
    pub fn new(part: i16, anchor: Point, initial: i16, ticks: u64) -> Self
    {
        Self { part,
               anchor,
               initial,
               next: ticks.saturating_add(20),
               point: anchor,
               hot: true }
    }

    /// Compute a candidate transition. The caller applies any fallible scroll
    /// or drawing admission before publishing it, so refusal preserves capture.
    pub fn poll(mut self, point: Point, down: bool, ticks: u64, scrollbar: Option<ScrollbarState>,
                reflow: u32, entry: DefinitionEntry, gate: TrapGate,
                hit: impl FnOnce(Point) -> Result<i16, Status>)
                -> Result<Transition, Status>
    {
        let mut action = Action::None;
        let highlight;
        if let Some(mut scrollbar) = scrollbar.filter(|_| self.part == SCROLL_THUMB)
        {
            if point != self.point
            {
                scrollbar.value = self.initial;
                let mut call = ControlCall { reflow,
                                             operation: TRACK_SCROLLBAR,
                                             scrollbar,
                                             anchor: self.anchor,
                                             point,
                                             ..Default::default() };
                super::prepare(&mut call, entry, gate)?;
                action = Action::Value(call.result as i16);
            }
            highlight = if down { self.part } else { 0 };
        }
        else
        {
            // Page geometry changes as the thumb passes a stationary pointer.
            let hot = if point == self.point
                         && !matches!(self.part, SCROLL_PAGE_UP | SCROLL_PAGE_DOWN)
            {
                self.hot
            }
            else
            {
                hit(point)? == self.part
            };
            self.hot = hot;
            highlight = if down && hot { self.part } else { 0 };
            if scrollbar.is_some()
            {
                if down && hot && ticks >= self.next
                {
                    action = Action::Step(self.part);
                    self.next = ticks.saturating_add(3);
                }
            }
            else if !down && hot
            {
                action = Action::Activate;
            }
        }
        self.point = point;
        Ok(Transition { tracking: down.then_some(self),
                        highlight,
                        action })
    }
}
