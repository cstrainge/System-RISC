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
                      let mut menus = match session.menus.try_borrow_mut()
                      {
                          Ok(menus) => menus,
                          Err(_) => return SERVICE_BUSY as i64
                      };
                      let at = match menus.iter()
                                          .position(|(id, ..)| *id == owner.owner().incarnation())
                      {
                          Some(at) => at,
                          None =>
                          {
                              if menus.try_reserve(1).is_err()
                              {
                                  return NO_MEMORY as i64;
                              }
                              let kinds =
                                  match (session.registry
                                                .get(crate::toolbox::handles::KindCode(*b"MENU")),
                                         session.registry
                                                .get(crate::toolbox::handles::KindCode(*b"MNST")))
                                  {
                                      (Ok(objects), Ok(storage)) => (objects, storage),
                                      (Err(error), _) | (_, Err(error)) =>
                                      {
                                          return crate::toolbox::handles::registry_status(&error)
                                                 as i64;
                                      }
                                  };
                              let manager =
                                  match crate::toolbox::menus::Manager::new(kinds,
                                                                            definition,
                                                                            super::host_gate)
                                  {
                                      Ok(manager) => manager,
                                      Err(status) => return status as i64
                                  };
                              let at = menus.len();
                              let system_bar =
                                  match crate::toolbox::menus::SystemBar::new(definition,
                                                                              super::host_gate,
                                                                              crate::toolbox::resources::manager::Manager::new(&session.resources, &session.application, session.fonts.resources()))
                                  {
                                      Ok(bar) => bar,
                                      Err(status) => return status as i64
                                  };
                              menus.push((owner.owner().incarnation(), manager, system_bar));
                              at
                          }
                      };
                      let resources =
                          crate::toolbox::resources::manager::Manager::new(&session.resources,
                                                                           &session.application,
                                                                           session.fonts
                                                                                  .resources());
                      let (_, manager, system_bar) = &mut menus[at];
                      manager.request(&owner, call, resources, system_bar).map_or_else(|status| {
                                                                                           status
                                                                                           as i64
                                                                                       },
                                                                                       |_| {
                                                                                           OK as i64
                                                                                       })
                  })
}

impl crate::toolbox::menus::lifetime::MenuHost for super::Session
{
    fn release_menu(&mut self, ticket: mac_object_table::domain::ReleasedResource)
                    -> Result<(), mac_object_table::domain::CompletionError>
    {
        let owner = ticket.key().owner().incarnation();
        match self.menus.get_mut().iter_mut().find(|(id, ..)| *id == owner) {
            Some((_, manager, _)) => manager.release(ticket),
            None => Err(<mac_object_table::domain::CompletionError as mac_object_table::registry::CleanupFailure>::registration_mismatch(ticket))
        }
    }

    fn release_menu_storage(&mut self, ticket: mac_object_table::domain::ReleasedResource)
                            -> Result<(), mac_object_table::domain::CompletionError>
    {
        let owner = ticket.key().owner().incarnation();
        match self.menus.get_mut().iter_mut().find(|(id, ..)| *id == owner) {
            Some((_, manager, _)) => manager.release_storage(ticket),
            None => Err(<mac_object_table::domain::CompletionError as mac_object_table::registry::CleanupFailure>::registration_mismatch(ticket))
        }
    }
}


#[derive(Clone, Copy)]
pub enum MeasureFault
{
    Refuse,
    ChangeInput
}
std::thread_local! {
    static MEASURE_FAULT: core::cell::Cell<Option<(MeasureFault, usize)>> = const { core::cell::Cell::new(None) };
}
/// Fail the second reflow measurement, after one result has already been staged.
/// The test guard restores its previous fault state even if an assertion unwinds.
pub fn with_measure_fault<R>(fault: MeasureFault, run: impl FnOnce() -> R) -> R
{
    MEASURE_FAULT.with(|slot| {
                     struct Restore<'a>(&'a core::cell::Cell<Option<(MeasureFault, usize)>>,
                                        Option<(MeasureFault, usize)>);
                     impl Drop for Restore<'_>
                     {
            fn drop(&mut self)
            {
                self.0.set(self.1);
            }
        }
                     let _restore = Restore(slot, slot.replace(Some((fault, 1))));
                     run()
                 })
}
/// # Safety
/// The production Menu Manager lends its exclusive, valid ABI call and state.
/// This fixture binding delegates to the real definition, then deliberately
/// violates only the tested output contract; no author implementation is replaced.
unsafe extern "C" fn definition(pointer: *mut DefinitionCall) -> i64
{
    let Some(call) = (unsafe { pointer.as_mut() })
    else
    {
        return BAD_ARGUMENT as i64;
    };
    let fault = if call.message == menus::MEASURE
    {
        MEASURE_FAULT.with(|slot| {
                         match slot.get()
                         {
                             Some((fault, 0)) => Some(fault),
                             Some((fault, left)) =>
                             {
                                 slot.set(Some((fault, left - 1)));
                                 None
                             }
                             None => None
                         }
                     })
    }
    else
    {
        None
    };
    if matches!(fault, Some(MeasureFault::Refuse))
    {
        return BAD_ARGUMENT as i64;
    }
    let reply = unsafe { crate::test_mdef::standard_menu_definition(call) };
    if reply >= 0 && matches!(fault, Some(MeasureFault::ChangeInput))
    {
        let Some(state) = (unsafe { call.object.cast::<menus::MenuDefinitionState>().as_mut() })
        else
        {
            return BAD_ARGUMENT as i64;
        };
        state.enabled ^= 1;
    }
    reply
}
