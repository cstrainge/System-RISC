//! Shared resident capture transitions. CDEFs retain all hit/thumb geometry.
use mac_abi::{BAD_ABI, Status, TrapGate, controls::*, geometry::Point};

use crate::toolbox::resources::definitions::DefinitionRef;

#[derive(Clone, Copy, PartialEq, Eq)]
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

    /// Same branches as poll_with; unchanged non-page input needs no author.
    pub(super) fn needs_author(&self, point: Point) -> bool
    {
        point != self.point || matches!(self.part, SCROLL_PAGE_UP | SCROLL_PAGE_DOWN)
    }

    /// Compute a candidate transition. The caller applies any fallible scroll
    /// or drawing admission before publishing it, so refusal preserves capture.
    pub fn poll(self, point: Point, down: bool, ticks: u64, scrollbar: Option<ScrollbarState>,
                reflow: u32, entry: DefinitionRef, gate: TrapGate,
                hit: impl FnOnce(Point) -> Result<i16, Status>)
                -> Result<Transition, Status>
    {
        let author = super::effects::Author::trusted(gate);
        super::effects::immediate(self.poll_with(point,
                                                 down,
                                                 ticks,
                                                 scrollbar,
                                                 reflow,
                                                 entry,
                                                 &author,
                                                 |point| core::future::ready(hit(point))))
    }

    pub(super) async fn poll_with<H, F>(mut self, point: Point, down: bool, ticks: u64,
                                        scrollbar: Option<ScrollbarState>, reflow: u32,
                                        entry: DefinitionRef, author: &super::effects::Author,
                                        hit: H)
                                        -> Result<Transition, Status>
        where H: FnOnce(Point) -> F,
              F: core::future::Future<Output = Result<i16, Status>>
    {
        let mut action = Action::None;
        let highlight;
        if let Some(mut scrollbar) = scrollbar.filter(|_| self.part == SCROLL_THUMB)
        {
            if point != self.point
            {
                scrollbar.value = self.initial;
                let call =
                    super::effects::DetachedCall::capture(&ControlCall { reflow,
                                                                         operation:
                                                                             TRACK_SCROLLBAR,
                                                                         scrollbar,
                                                                         anchor: self.anchor,
                                                                         point,
                                                                         ..Default::default() })?;
                let (call, _) = author.prepare(call, entry, None).await?;
                action = Action::Value(i16::try_from(call.result).map_err(|_| BAD_ABI)?);
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
                hit(point).await? == self.part
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
