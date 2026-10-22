//! Trusted input/scheduler fixture around the production drag service. Tests
//! still create offers, respond, take data and cancel through the public SDK.
use std::cell::Cell;

use super::*;
std::thread_local! { static THREAD: Cell<u64> = const { Cell::new(1) }; }
pub fn with_thread<R>(thread: u64, run: impl FnOnce() -> R) -> R
{
    struct Restore(u64);
    impl Drop for Restore
    {
        fn drop(&mut self)
        {
            THREAD.set(self.0);
        }
    }
    let _restore = Restore(THREAD.replace(thread));
    run()
}
fn drain(s: &mut Session)
{
    while let Some(notice) = s.drag.next_notice()
    {
        s.drag_notices.push(notice);
    }
}
pub(super) fn request(s: &mut Session, call: &mut mac_abi::drag::Call, input: &[u8],
                      output: &mut [u8])
                      -> Result<(), Status>
{
    if matches!(call.operation,
                mac_abi::drag::ACCEPT
                | mac_abi::drag::READ
                | mac_abi::drag::ACKNOWLEDGE
                | mac_abi::drag::ABANDON)
       && call.receiver == 0
    {
        call.receiver = s.drag.receiver_at(call.window).ok_or(NOT_FOUND)?;
    }
    let result = s.drag.request(files::current_owner(), THREAD.get(), call, input, output, 0);
    drain(s);
    result
}
pub(super) struct Services<'a>(pub(super) &'a mut Session);
impl crate::toolbox::classic::Services for Services<'_>
{
    fn drag(&mut self, call: &mut mac_abi::drag::Call, input: &[u8], output: &mut [u8])
            -> Result<(), Status>
    {
        request(self.0, call, input, output)
    }
}
pub fn receiver(owner: u64, thread: u64, window: WindowId)
{
    SESSION.with(|s| {
               s.borrow_mut()
                .as_mut()
                .unwrap()
                .drag
                .ensure_receiver(owner, thread, window.0)
                .unwrap()
           });
}
pub fn motion(point: Point, modifiers: u16, window: WindowId, when: u32)
{
    SESSION.with(|s| {
               let mut s = s.borrow_mut();
               let s = s.as_mut().unwrap();
               s.drag.motion(point, modifiers, s.drag.receiver_at(window.0), when);
               drain(s);
           });
}
pub fn release(point: Point, modifiers: u16, window: WindowId, when: u32)
{
    SESSION.with(|s| {
               let mut s = s.borrow_mut();
               let s = s.as_mut().unwrap();
               s.drag.release(point, modifiers, s.drag.receiver_at(window.0), when);
               drain(s);
           });
}
pub fn tick(when: u32)
{
    SESSION.with(|s| {
               let mut s = s.borrow_mut();
               let s = s.as_mut().unwrap();
               s.drag.tick(when);
               drain(s);
           });
}
pub fn feedback() -> Option<Rect>
{
    SESSION.with(|s| s.borrow().as_ref().unwrap().drag.feedback())
}
