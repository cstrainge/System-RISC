//! Menu tests share the production dispatch and the common owner/registry fixture.
use mac_abi::*;

pub fn with_resources<R>(system: &[u8], application: &[u8], run: impl FnOnce(&SystemServices) -> R)
                         -> R
{
    super::with_resource_forks(system, application, run).expect("valid menu fixture resource forks")
}

/// # Safety
/// The host service gate lends a valid, exclusive request exactly as the native
/// trap boundary does. Definition calls may borrow immutable font services while
/// this per-owner Menu Manager is mutably borrowed.
pub(super) unsafe fn dispatch(pointer: *mut (), size: usize) -> i64
{
    let call = match unsafe { super::request::<menus::MenuCall>(pointer, size) }
    {
        Ok(call) => call,
        Err(status) => return status as i64
    };
    let owner = match super::inspection::caller_ledger(super::files::current_owner())
    {
        Ok(owner) => owner,
        Err(status) => return status as i64
    };
    super::SESSION.with(|slot| {
        let session = slot.borrow();
        let session = session.as_ref().expect("live host menu session");
        let mut menus = match session.menus.try_borrow_mut() { Ok(menus) => menus, Err(_) => return SERVICE_BUSY as i64 };
        let at = match menus.iter().position(|(id, _)| *id == owner.owner().incarnation()) {
            Some(at) => at,
            None => {
                if menus.try_reserve(1).is_err() { return NO_MEMORY as i64; }
                let kinds = match (session.registry.get(crate::toolbox::handles::KindCode(*b"MENU")),
                                   session.registry.get(crate::toolbox::handles::KindCode(*b"MNST"))) {
                    (Ok(objects), Ok(storage)) => (objects, storage),
                    (Err(error), _) | (_, Err(error)) => return crate::toolbox::handles::registry_status(&error) as i64
                };
                let manager = match crate::toolbox::menus::Manager::new(kinds, crate::test_mdef::standard_menu_definition, super::host_gate) {
                    Ok(manager) => manager, Err(status) => return status as i64
                };
                let at = menus.len();
                menus.push((owner.owner().incarnation(), manager)); at
            }
        };
        let resources = crate::toolbox::resources::manager::Manager::new(&session.resources, &session.application, session.fonts.resources());
        menus[at].1.request(&owner, call, resources).map_or_else(|status| status as i64, |_| OK as i64)
    })
}

impl crate::toolbox::menus::lifetime::MenuHost for super::Session
{
    fn release_menu(&mut self, ticket: mac_object_table::domain::ReleasedResource)
                    -> Result<(), mac_object_table::domain::CompletionError>
    {
        let owner = ticket.key().owner().incarnation();
        match self.menus.get_mut().iter_mut().find(|(id, _)| *id == owner) {
            Some((_, manager)) => manager.release(ticket),
            None => Err(<mac_object_table::domain::CompletionError as mac_object_table::registry::CleanupFailure>::registration_mismatch(ticket))
        }
    }

    fn release_menu_storage(&mut self, ticket: mac_object_table::domain::ReleasedResource)
                            -> Result<(), mac_object_table::domain::CompletionError>
    {
        let owner = ticket.key().owner().incarnation();
        match self.menus.get_mut().iter_mut().find(|(id, _)| *id == owner) {
            Some((_, manager)) => manager.release_storage(ticket),
            None => Err(<mac_object_table::domain::CompletionError as mac_object_table::registry::CleanupFailure>::registration_mismatch(ticket))
        }
    }
}
