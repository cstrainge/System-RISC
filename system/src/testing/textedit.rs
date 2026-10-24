//! Trusted host contexts use the same registered per-app TextEdit managers.
use mac_abi::*;

/// # Safety
/// The host gate lends valid exclusive request/buffer storage for the call.
pub(super) unsafe fn dispatch(number: u32, pointer: *mut (), size: usize) -> i64
{
    let owner = match super::inspection::caller_ledger(super::files::current_owner())
    {
        Ok(owner) => owner,
        Err(status) => return status as i64
    };
    super::SESSION.with(|slot| {
        let Ok(mut session) = slot.try_borrow_mut()
        else
        {
            return SERVICE_BUSY as i64;
        };
        let Some(session) = session.as_mut()
        else
        {
            return NOT_SUPPORTED as i64;
        };
        let id = owner.owner().incarnation();
        let at = match session.textedit.iter().position(|(subject, _)| *subject == id)
        {
            Some(at) => at,
            None =>
            {
                if session.textedit.try_reserve(1).is_err()
                {
                    return NO_MEMORY as i64;
                }
                let kinds =
                    match (session.registry.get(crate::toolbox::handles::KindCode(*b"TEDT")),
                           session.registry.get(crate::toolbox::handles::KindCode(*b"TESV")))
                    {
                        (Ok(objects), Ok(state)) => (objects, state),
                        (Err(error), _) | (_, Err(error)) =>
                        {
                            return crate::toolbox::handles::registry_status(&error) as i64;
                        }
                    };
                let manager = match crate::toolbox::textedit::Manager::new(kinds, owner)
                {
                    Ok(manager) => manager,
                    Err(status) => return status as i64
                };
                let at = session.textedit.len();
                session.textedit.push((id, manager));
                at
            }
        };
        let manager = &mut session.textedit[at].1;
        unsafe {
            crate::toolbox::textedit::native::dispatch_with(number, pointer, size, |request| {
                match request
                {
                    crate::toolbox::textedit::native::Request::Edit(call, input, output) =>
                    {
                        crate::toolbox::textedit::clipboard::request(manager, &mut session.scrap,
                                                                     call, input, output,
                                                                     &session.fonts)
                    }
                    crate::toolbox::textedit::native::Request::Display(call) =>
                    {
                        manager.display_request(call)
                    }
                    crate::toolbox::textedit::native::Request::Font(call) =>
                    {
                        manager.font_request(call, &session.fonts)
                    }
                }
            })
        }
    })
}
impl crate::toolbox::textedit::lifetime::EditorHost for super::Session
{
    fn release_editor(&mut self, ticket: mac_object_table::domain::ReleasedResource, storage: bool)
                      -> Result<(), mac_object_table::domain::CompletionError>
    {
        let owner = ticket.key().owner().incarnation();
        match self.textedit.iter_mut().find(|(subject,_)|*subject==owner) {
            Some((_,manager)) => manager.release(ticket,storage),
            None => Err(<mac_object_table::domain::CompletionError as mac_object_table::registry::CleanupFailure>::registration_mismatch(ticket))
        }
    }
}
