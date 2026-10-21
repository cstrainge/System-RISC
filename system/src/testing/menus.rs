//! Real resource-based Menu Manager fixture. No behavioral expectations live here.
use core::cell::RefCell;

use mac_abi::*;
struct Session
{
    memory: RefCell<crate::toolbox::memory::Manager>,
    menus:  RefCell<crate::toolbox::menus::Manager>,
    system: std::vec::Vec<mac_formats::resource::Resource>,
    app:    std::vec::Vec<mac_formats::resource::Resource>,
    fonts:  crate::toolbox::quickdraw::fonts::FontManager
}
std::thread_local! {static SESSION:RefCell<Option<Session>>=const{RefCell::new(None)};}
struct Scope;
impl Drop for Scope
{
    fn drop(&mut self)
    {
        SESSION.with(|s| {
                   s.borrow_mut().take();
               });
    }
}
pub fn with_resources<R>(system: &[u8], application: &[u8], run: impl FnOnce(&SystemServices) -> R)
                         -> R
{
    let mut system = mac_formats::resource::decode(system).expect("system fork");
    let app = mac_formats::resource::decode(application).expect("application fork");
    let fonts = crate::toolbox::quickdraw::fonts::FontManager::take_resources(&mut system);
    SESSION.with(|slot|{assert!(slot.borrow().is_none());*slot.borrow_mut()=Some(Session{memory:RefCell::new(Default::default()),menus:RefCell::new(crate::toolbox::menus::Manager::new(crate::test_mdef::standard_menu_definition,gate)),system,app,fonts});});
    let _scope = Scope;
    let services = SystemServices { header:       Header::for_type::<SystemServices>(),
                                    trap:         gate,
                                    capabilities: CAP_GRAPHICS };
    run(&services)
}
unsafe extern "C" fn gate(number: u32, pointer: *mut (), size: usize) -> i64
{
    if number == traps::SURFACE
    {
        return unsafe { crate::toolbox::quickdraw::surface_gate(number, pointer, size) };
    }
    SESSION.with(|slot| {
               let slot = slot.borrow();
               let Some(session) = slot.as_ref()
               else
               {
                   return NOT_SUPPORTED as i64;
               };
               if number == traps::MEMORY
               {
                   let Ok(mut memory) = session.memory.try_borrow_mut()
                   else
                   {
                       return SERVICE_BUSY as i64;
                   };
                   return unsafe {
                       crate::toolbox::memory::native::dispatch(&mut memory, number, pointer, size)
                   };
               }
               if number == traps::FONT
               {
                   return unsafe {
                       crate::toolbox::quickdraw::native::dispatch(number,
                                                                   pointer,
                                                                   size,
                                                                   Some(&session.fonts))
                   };
               }
               if number != traps::MENU
               {
                   return NOT_SUPPORTED as i64;
               }
               if pointer.is_null()
                  || size != core::mem::size_of::<menus::MenuCall>()
                  || !(pointer as usize).is_multiple_of(core::mem::align_of::<menus::MenuCall>())
               {
                   return BAD_ARGUMENT as i64;
               }
               let Ok(mut menus) = session.menus.try_borrow_mut()
               else
               {
                   return SERVICE_BUSY as i64;
               };
               let resources =
                   crate::toolbox::resources::manager::Manager::new(&session.system,
                                                                    &session.app,
                                                                    session.fonts.resources());
               menus.request(unsafe { &mut *pointer.cast::<menus::MenuCall>() }, resources)
                    .map_or_else(|e| e as i64, |_| 0)
           })
}
