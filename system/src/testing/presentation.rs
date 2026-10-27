//! Trusted framebuffer/input transport around the real resident cursor service.
//! Cursor selection and visibility still enter through public Toolbox calls.
use mac_abi::{Point, Rect};

pub fn pointer(point: Point)
{
    super::SESSION.with(|slot| {
                      let mut slot = slot.borrow_mut();
                      let session = slot.as_mut().unwrap();
                      session.events
                             .observe_input(crate::toolbox::events::input::Input::Move(point), 0);
                      session.cursors.mouse_moved(point);
                  });
}

pub fn present(scene: &[u32], front: &mut [u32], width: usize, height: usize,
               rects: impl IntoIterator<Item = Rect>)
               -> std::vec::Vec<Rect>
{
    super::SESSION.with(|slot| {
                      let mut slot = slot.borrow_mut();
                      let session = slot.as_mut().unwrap();
                      let sprite = session.cursors.sprite(session.events.mouse, None);
                      rects.into_iter()
                           .filter_map(|rect| {
                               crate::toolbox::quickdraw::presentation::present_rect(scene, front,
                                                                                     width, height,
                                                                                     rect, sprite)
                           })
                           .collect()
                  })
}

/// Trusted presentation transport. Authoritative selection/lifetime operations
/// remain public SDK calls; this state only models the previously shown pixels.
#[derive(Default)]
pub struct DragFrame
{
    previous_cursor:   Rect,
    previous_feedback: crate::toolbox::quickdraw::feedback::Feedback
}
impl DragFrame
{
    pub fn present(&mut self, scene: &[u32], front: &mut [u32], width: usize, height: usize,
                   damage: impl IntoIterator<Item = Rect>, outline: Option<(&[Rect], i32, i32)>,
                   cursor_changed: bool)
                   -> Option<(usize, std::vec::Vec<Rect>)>
    {
        use crate::toolbox::quickdraw::{feedback::{Clip, Feedback},
                                        presentation::Outline};
        let feedback = match outline
        {
            Some((pieces, dx, dy)) =>
            {
                Feedback::outline(Outline::new(pieces, dx, dy)?, Clip::Screen)
            }
            None => Feedback::default()
        };
        self.publish(scene, front, width, height, damage, feedback, cursor_changed)
    }

    /// Feedback geometry is trusted capture input. Object lifecycle and cursor
    /// state still come from the public SDK and real resident services.
    pub fn present_feedback(&mut self, scene: &[u32], front: &mut [u32], width: usize,
                            height: usize, damage: impl IntoIterator<Item = Rect>,
                            outline: Option<(Rect, Option<&[Rect]>)>,
                            highlight: Option<(Rect, Option<&[Rect]>)>, cursor_changed: bool)
                            -> Option<(usize, std::vec::Vec<Rect>)>
    {
        use crate::toolbox::quickdraw::{feedback::{Clip, ClippedOutline, Feedback, Highlight},
                                        presentation::Outline};
        let clip = |rects: Option<&[Rect]>, previous: Option<&Clip>| {
            match rects
            {
                Some(rects) => Clip::rectangles(rects, previous),
                None => Some(Clip::Screen)
            }
        };
        let outline = match outline
        {
            Some((bounds, visible)) =>
            {
                Some(ClippedOutline { shape: Outline::new(&[bounds], 0, 0)?,
                                      clip:  clip(visible,
                                                  self.previous_feedback
                                                      .outline
                                                      .as_ref()
                                                      .map(|v| &v.clip))? })
            }
            None => None
        };
        let highlight = match highlight
        {
            Some((bounds, visible)) =>
            {
                Some(Highlight { bounds,
                                 clip: clip(visible,
                                            self.previous_feedback
                                                .highlight
                                                .as_ref()
                                                .map(|v| &v.clip))? })
            }
            None => None
        };
        self.publish(scene,
                     front,
                     width,
                     height,
                     damage,
                     Feedback { outline,
                                highlight },
                     cursor_changed)
    }

    fn publish(&mut self, scene: &[u32], front: &mut [u32], width: usize, height: usize,
               damage: impl IntoIterator<Item = Rect>,
               feedback: crate::toolbox::quickdraw::feedback::Feedback, cursor_changed: bool)
               -> Option<(usize, std::vec::Vec<Rect>)>
    {
        use crate::toolbox::quickdraw::presentation::{Overlays, PresentEvent, present_frame};
        super::SESSION.with(|slot| {
                          let mut slot = slot.borrow_mut();
                          let session = slot.as_mut().unwrap();
                          let cursor = session.cursors.sprite(session.events.mouse, None);
                          let mut copies = std::vec::Vec::new();
                          let mut observed_changes = 0;
                          let changed =
                              present_frame(scene,
                                            front,
                                            width,
                                            height,
                                            damage,
                                            Overlays { previous_cursor: self.previous_cursor,
                                                       cursor,
                                                       cursor_changed,
                                                       previous_feedback:
                                                           Some(&self.previous_feedback),
                                                       feedback: Some(&feedback) },
                                            |event| {
                                                match event
                                                {
                                                    PresentEvent::Copied(rect) => copies.push(rect),
                                                    PresentEvent::OutlineChanged(pixels) =>
                                                    {
                                                        observed_changes = pixels
                                                    }
                                                    PresentEvent::CopyStarted
                                                    | PresentEvent::OutlineStarted => ()
                                                }
                                            })?;
                          assert_eq!(changed, observed_changes);
                          self.previous_cursor = cursor.map_or(Rect::ZERO, |c| c.bounds());
                          self.previous_feedback = feedback;
                          Some((changed, copies))
                      })
    }
}

/// Trusted clock transport, invoking the actual resident animation step. Setup,
/// palette admission and explicit advances still use the public cursor SDK.
pub fn cursor_tick(ticks: u32) -> bool
{
    super::SESSION.with(|slot| slot.borrow_mut().as_mut().unwrap().cursors.animation_tick(ticks))
}
