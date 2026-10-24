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
