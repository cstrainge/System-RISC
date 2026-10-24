//! Trusted input/scheduler fixture around the production drag service. Tests
//! still create offers, respond, take data and cancel through the public SDK.
use std::cell::Cell;

use mac_traps::VolumeId;

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
    let source_read_only = crate::toolbox::drag::source_read_only(&mut s.files, call, input)?;
    crate::toolbox::drag::validate_target(&mut s.files, call)?;
    if matches!(call.operation,
                mac_abi::drag::ACCEPT
                | mac_abi::drag::READ
                | mac_abi::drag::ACKNOWLEDGE
                | mac_abi::drag::CANCEL_RECEIVE
                | mac_abi::drag::ABANDON)
       && call.receiver == 0
    {
        call.receiver = s.drag.receiver_at(call.window).ok_or(NOT_FOUND)?;
    }
    let result = s.drag.request(files::current_owner(), THREAD.get(), call, input, output, 0);
    if result.is_ok() && source_read_only
    {
        s.drag.source_read_only(call.id);
    }
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
pub fn highlight() -> Option<(u64, Rect)>
{
    SESSION.with(|s| s.borrow().as_ref().unwrap().drag.highlight())
}
/// Admit a descriptor without entering provider code. Negotiation must consult
/// resident capability state, so a driver callback is neither needed nor legal.
pub fn provider_volume(read_only: bool) -> VolumeId
{
    provider_volume_for_export(read_only, 1)
}
pub fn provider_volume_for_export(read_only: bool, export: u64) -> VolumeId
{
    use mac_filesystem_interface as fs;
    SESSION.with(|slot| {
        let mut slot = slot.borrow_mut();
        let files = &mut slot.as_mut().unwrap().files;
        let id =
            files.reserve_provider_mount(1,
                                         crate::toolbox::files::TestProviderSource::Export(export))
                 .unwrap();
        let descriptor = fs::Mount { capabilities:       fs::READ
                                                         | if read_only { 0 } else { fs::WRITE },
                                     filesystem:         *b"TEST",
                                     filesystem_version: 1,
                                     capacity:           None,
                                     root:               fs::Metadata { id:              2,
                                                                        parent:          0,
                                                                        directory:       true,
                                                                        data_length:     0,
                                                                        resource_length: 0,
                                                                        created:         0,
                                                                        modified:        0,
                                                                        backup:          None,
                                                                        child_count:     0,
                                                                        attributes:      0,
                                                                        finder_info:     [0; 32],
                                                                        revision:        1,
                                                                        name:            "Provider",
                                                                        comment:         "" } };
        let mut bytes = [0; fs::MAX_MOUNT_BYTES];
        let length = descriptor.encode(&mut bytes).unwrap();
        files.publish_provider_mount(id, &bytes[..length]).unwrap();
        VolumeId(id)
    })
}
