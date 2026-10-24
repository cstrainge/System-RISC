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
    previous_cursor:  Rect,
    previous_outline: Option<crate::toolbox::quickdraw::presentation::WindowOutline>
}
impl DragFrame
{
    pub fn present(&mut self, scene: &[u32], front: &mut [u32], width: usize, height: usize,
                   damage: impl IntoIterator<Item = Rect>, outline: Option<(&[Rect], i32, i32)>,
                   cursor_changed: bool)
                   -> Option<(usize, std::vec::Vec<Rect>)>
    {
        use crate::toolbox::quickdraw::presentation::{Overlays, PresentEvent, WindowOutline,
                                                      present_frame};
        let outline = match outline
        {
            Some((pieces, dx, dy)) => Some(WindowOutline::new(pieces, dx, dy)?),
            None => None
        };
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
                                                       previous_outline: self.previous_outline,
                                                       outline },
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
                          self.previous_outline = outline;
                          Some((changed, copies))
                      })
    }
}
