//! Host userspace conformance harness, available only under `test-support`.
//! Calls enter the same resident service dispatchers as native userspace; this
//! module supplies lifetime/scoping and the host adapter, never canned results.
#[cfg(test)]
#[path = "../../../crates/mac-toolbox/tests/support/allocations.rs"]
pub(crate) mod allocations;
pub mod shared;
pub mod menus;
pub mod scrap;
pub mod scrap_extension;
pub mod drag;
pub mod inspection;
pub mod presentation;
#[path = "../boot_screen/strip.rs"]
pub mod extension_startup;
pub mod power;
mod worlds;
mod textedit;
use core::cell::RefCell;
use std::vec::Vec;

use mac_abi::{traps::*, *};

struct Session
{
    registry:      crate::toolbox::handles::Registry<Session>,
    menus:         RefCell<std::vec::Vec<(u64, crate::toolbox::menus::Manager)>>,
    inspection:    inspection::Domains,
    modules:       crate::drivers::Registry,
    scopes:        files::scopes::Scopes,
    drag:          crate::toolbox::drag::Manager,
    drag_notices:  std::vec::Vec<crate::toolbox::drag::Notice>,
    standard_file: crate::toolbox::standard_file::Manager,
    worlds:        crate::toolbox::quickdraw::worlds::Worlds,
    scroll_boxes:  crate::toolbox::controls::scrollbox::Manager,
    // Definitions can query immutable font services while the Control Manager
    // is borrowed. Keep that manager's mutable borrow narrower than Session.
    controls:      RefCell<crate::toolbox::controls::objects::Manager>,
    crashes:       crate::toolbox::crashes::Manager,
    events:        crate::toolbox::events::EventManager,
    scrap:         crate::toolbox::scrap::Manager,
    textedit:      std::vec::Vec<(u64, crate::toolbox::textedit::Manager)>,
    cursors:       crate::toolbox::quickdraw::cursors::Cursors,
    started:       std::time::Instant,
    memory:        crate::toolbox::memory::Manager,
    files:         crate::toolbox::files::Manager,
    power:         power::State,
    fonts:         crate::toolbox::quickdraw::fonts::FontManager,
    resources:     std::vec::Vec<mac_formats::resource::Resource>,
    application:   std::vec::Vec<mac_formats::resource::Resource>
}
impl Default for Session
{
    fn default() -> Self
    {
        let mut resources = std::vec::Vec::new();
        for (kind, id, name, data) in
            [(*b"FOND",
              0,
              Some("Chicago"),
              include_bytes!("../../assets/fonts/FOND-0.bin").as_slice()),
             (*b"FONT", 0, None, include_bytes!("../../assets/fonts/FONT-0.bin").as_slice()),
             (*b"FONT", 12, None, include_bytes!("../../assets/fonts/FONT-12.bin").as_slice())]
        {
            resources.push(mac_formats::resource::Resource { kind,
                                                             id,
                                                             name: name.map(|n| {
                                                                           n.as_bytes().to_vec()
                                                                       }),
                                                             attributes: 0,
                                                             data: data.to_vec() });
        }
        let fonts = crate::toolbox::quickdraw::fonts::FontManager::take_resources(&mut resources)
            .expect("bundled fixture font catalog backing is admitted");
        let registrar = crate::toolbox::handles::OwnerRef::try_new(
            mac_object_table::domain::OwnerIndex::issue(mac_object_table::domain::DomainKind::Module).unwrap()).unwrap();
        let mut registry = crate::toolbox::handles::Registry::<Session>::new();
        for kind in crate::toolbox::files::pins::Registration::ALL
        {
            registry.register(&registrar, kind.specification(), kind).unwrap();
        }
        let scopes = files::scopes::Scopes::new(&mut registry, &registrar);
        let controls = registry.register(&registrar,
                                         crate::toolbox::controls::objects::specification(),
                                         crate::toolbox::handles::cleanup::Complete(crate::toolbox::controls::objects::ReleaseControls))
                               .unwrap();
        let storage = registry.register(&registrar,
                                        crate::toolbox::controls::objects::storage_specification(),
                                        crate::toolbox::handles::cleanup::Complete(crate::toolbox::controls::objects::ReleaseStorage))
                              .unwrap();
        let scroll_boxes =
            registry.register(&registrar,
                              crate::toolbox::controls::scrollbox::specification(false),
                              crate::toolbox::handles::cleanup::Complete(crate::toolbox::controls::scrollbox::ReleaseScrollBoxes))
                    .unwrap();
        let scroll_storage =
            registry.register(&registrar,
                              crate::toolbox::controls::scrollbox::specification(true),
                              crate::toolbox::handles::cleanup::Complete(crate::toolbox::controls::scrollbox::ReleaseStorage))
                    .unwrap();
        let dialogs = registry.register(&registrar,
                                        crate::toolbox::standard_file::specification(false),
                                        crate::toolbox::handles::cleanup::Complete(crate::toolbox::standard_file::ReleaseDialogs))
                              .unwrap();
        let dialog_storage = registry.register(&registrar,
                                               crate::toolbox::standard_file::specification(true),
                                               crate::toolbox::handles::cleanup::Complete(crate::toolbox::standard_file::ReleaseStorage))
                                     .unwrap();
        let worlds = registry.register(&registrar,
                                       crate::toolbox::quickdraw::worlds::specification(false),
                                       crate::toolbox::handles::cleanup::Complete(crate::toolbox::quickdraw::worlds::ReleaseWorlds))
                             .unwrap();
        let world_storage =
            registry.register(&registrar,
                              crate::toolbox::quickdraw::worlds::specification(true),
                              crate::toolbox::handles::cleanup::Complete(crate::toolbox::quickdraw::worlds::ReleaseStorage))
                    .unwrap();
        registry.register(&registrar, crate::toolbox::menus::lifetime::specification(false),
            crate::toolbox::handles::cleanup::Complete(crate::toolbox::menus::lifetime::ReleaseMenus)).unwrap();
        registry.register(&registrar, crate::toolbox::menus::lifetime::specification(true),
            crate::toolbox::handles::cleanup::Complete(crate::toolbox::menus::lifetime::ReleaseStorage)).unwrap();
        let memory = registry.register(&registrar,
                                       crate::toolbox::memory::lifetime::specification(false),
                                       crate::toolbox::memory::lifetime::ReleaseBlocks)
                             .unwrap();
        let memory_storage =
            registry.register(&registrar,
                              crate::toolbox::memory::lifetime::specification(true),
                              crate::toolbox::memory::lifetime::ReleaseStorage)
                    .unwrap();
        registry.register(&registrar, crate::toolbox::textedit::lifetime::specification(false),
            crate::toolbox::handles::cleanup::Complete(crate::toolbox::textedit::lifetime::ReleaseEditors)).unwrap();
        registry.register(&registrar, crate::toolbox::textedit::lifetime::specification(true),
            crate::toolbox::handles::cleanup::Complete(crate::toolbox::textedit::lifetime::ReleaseStorage)).unwrap();
        registry.register(&registrar,
                          crate::toolbox::files::watch::lifetime::specification(false),
                          crate::toolbox::files::watch::lifetime::ReleaseWatches)
                .unwrap();
        registry.register(&registrar,
                          crate::toolbox::files::watch::lifetime::specification(true),
                          crate::toolbox::files::watch::lifetime::ReleaseStorage)
                .unwrap();
        registry.register(&registrar,
                          crate::toolbox::files::forks::lifetime::specification(false),
                          crate::toolbox::files::forks::lifetime::ReleaseForks)
                .unwrap();
        registry.register(&registrar,
                          crate::toolbox::files::forks::lifetime::specification(true),
                          crate::toolbox::files::forks::lifetime::ReleaseStorage)
                .unwrap();
        registry.register(&registrar,
                          crate::toolbox::files::write::lifetime::specification(false),
                          crate::toolbox::files::write::lifetime::ReleaseWrites)
                .unwrap();
        registry.register(&registrar,
                          crate::toolbox::files::write::lifetime::specification(true),
                          crate::toolbox::files::write::lifetime::ReleaseStorage)
                .unwrap();
        registry.register(&registrar,
                          crate::toolbox::files::streams::lifetime::specification(false),
                          crate::toolbox::files::streams::lifetime::ReleaseStreams)
                .unwrap();
        registry.register(&registrar,
                          crate::toolbox::files::streams::lifetime::specification(true),
                          crate::toolbox::files::streams::lifetime::ReleaseStorage)
                .unwrap();
        registry.register(&registrar,
                          crate::toolbox::files::groups::lifetime::specification(false),
                          crate::toolbox::files::groups::lifetime::ReleaseGroups)
                .unwrap();
        registry.register(&registrar,
                          crate::toolbox::files::groups::lifetime::specification(true),
                          crate::toolbox::files::groups::lifetime::ReleaseStorage)
                .unwrap();
        use crate::toolbox::drag::lifetime::{Registration, ReleaseKind};
        for kind in [Registration::Sessions,
                     Registration::SessionStorage,
                     Registration::Receivers,
                     Registration::ReceiverStorage]
        {
            registry.register(&registrar, kind.specification(), ReleaseKind(kind)).unwrap();
        }
        let drag = crate::toolbox::drag::Manager::new(
            (registry.get(crate::toolbox::handles::KindCode(*b"DRAG")).unwrap(),
             registry.get(crate::toolbox::handles::KindCode(*b"DRST")).unwrap()),
            (registry.get(crate::toolbox::handles::KindCode(*b"DRCV")).unwrap(),
             registry.get(crate::toolbox::handles::KindCode(*b"DRVS")).unwrap())).unwrap();
        let interface_kind =
            registry.register(&registrar,
                              crate::drivers::registry::interfaces::specification(false),
                              crate::drivers::registry::interfaces::ReleaseInterfaces(false))
                    .unwrap();
        let interface_storage =
            registry.register(&registrar,
                              crate::drivers::registry::interfaces::specification(true),
                              crate::drivers::registry::interfaces::ReleaseInterfaces(true))
                    .unwrap();
        let mut modules = crate::drivers::Registry::new(mac_module::Target::RVA23U64,
                                                    registrar.clone(),
                                                    [interface_kind, interface_storage,
            registry.register(&registrar, crate::drivers::registry::dependencies::specification(false),
                crate::drivers::registry::dependencies::ReleaseDependencies(false)).unwrap(),
            registry.register(&registrar, crate::drivers::registry::dependencies::specification(true),
                crate::drivers::registry::dependencies::ReleaseDependencies(true)).unwrap(),
            registry.register(&registrar, crate::drivers::registry::requests::specification(false),
                crate::drivers::registry::requests::ReleaseRequests(false)).unwrap(),
            registry.register(&registrar, crate::drivers::registry::requests::specification(true),
                crate::drivers::registry::requests::ReleaseRequests(true)).unwrap()]).unwrap();
        let io_root = registry.register(&registrar,
                                        crate::drivers::jobs::subjects::specification(false),
                                        crate::drivers::registry::fixture::ReleaseIo(false))
                              .unwrap();
        let io_backing = registry.register(&registrar,
                                           crate::drivers::jobs::subjects::specification(true),
                                           crate::drivers::registry::fixture::ReleaseIo(true))
                                 .unwrap();
        modules.fixture_operations =
            Some(crate::drivers::jobs::subjects::Operations::new((io_root, io_backing)).unwrap());
        Self { worlds: crate::toolbox::quickdraw::worlds::Worlds::new((worlds, world_storage)).unwrap(),
               registry,
               menus: RefCell::new(std::vec::Vec::new()),
               inspection: inspection::Domains::new(),
               modules,
               scopes,
               drag,
               drag_notices: std::vec::Vec::new(),
               controls:
                   RefCell::new(crate::toolbox::controls::objects::Manager::new((controls,
                                                                                 storage)).unwrap()),
               scroll_boxes: crate::toolbox::controls::scrollbox::Manager::new((scroll_boxes, scroll_storage)).unwrap(),
               standard_file: crate::toolbox::standard_file::Manager::new((dialogs, dialog_storage)).unwrap(),
               crashes: Default::default(),
               events: crate::toolbox::events::EventManager::new(),
               scrap: Default::default(),
               textedit: std::vec::Vec::new(),
               cursors: Default::default(),
               started: std::time::Instant::now(),
               memory: crate::toolbox::memory::Manager::new((memory, memory_storage)).unwrap(),
               files: Default::default(),
               power: Default::default(),
               fonts,
               resources,
               application: std::vec::Vec::new() }
    }
}
impl crate::toolbox::controls::objects::ControlHost for Session
{
    fn managed_controls(&mut self) -> &mut crate::toolbox::controls::objects::Manager
    {
        self.controls.get_mut()
    }
}
impl crate::toolbox::controls::scrollbox::ScrollBoxHost for Session
{
    fn managed_scroll_boxes(&mut self) -> &mut crate::toolbox::controls::scrollbox::Manager
    {
        &mut self.scroll_boxes
    }
}
std::thread_local! {static SESSION:RefCell<Option<Session>>=const {RefCell::new(None)};}
impl crate::toolbox::standard_file::FileDialogHost for Session
{
    fn file_dialogs(&mut self) -> &mut crate::toolbox::standard_file::Manager
    {
        &mut self.standard_file
    }
}
struct Scope;
impl Drop for Scope
{
    fn drop(&mut self)
    {
        SESSION.with(|slot| {
            if let Some(mut session) = slot.borrow_mut().take() {
                let count = session.inspection.iter().count();
                for index in 0..count {
                    let owner = session.inspection.iter().nth(index).unwrap().ledger().clone();
                    let result = mac_object_table::walk::drain(&mut session, &owner,
                        |session, key| session.registry.dispatch(key.kind()),
                        |key, error| std::eprintln!("Host fixture cleanup retained {key:?}: {error}"));
                    if let Err(error) = result { std::eprintln!("Host fixture cleanup barrier retained: {error}"); }
                }
            }
        });
    }
}
/// Expose a live native service table for one host test. Each test thread gets
/// fresh resident state, disposed on return or unwind. Nested sessions are a
/// harness error; nested service calls within a session remain supported.
pub fn with_services<R>(run: impl FnOnce(&SystemServices) -> R) -> R
{
    SESSION.with(|slot| {
               let mut slot = slot.borrow_mut();
               assert!(slot.is_none(), "nested conformance session");
               *slot = Some(Session::default());
           });
    let _scope = Scope;
    let services = SystemServices { header:       Header::for_type::<SystemServices>(),
                                    trap:         host_gate,
                                    capabilities: CAP_GRAPHICS };
    run(&services)
}
unsafe fn request<'a, T>(pointer: *mut (), size: usize) -> Result<&'a mut T, Status>
{
    if pointer.is_null()
       || size != core::mem::size_of::<T>()
       || !(pointer as usize).is_multiple_of(core::mem::align_of::<T>())
    {
        return Err(BAD_ARGUMENT);
    }
    Ok(unsafe { &mut *pointer.cast::<T>() })
}
/// # Safety
/// Request and nested payloads must borrow live memory for their declared sizes,
/// exactly as for the native service gate. Public SDKs establish these borrows.
unsafe extern "C" fn host_gate(number: u32, pointer: *mut (), size: usize) -> i64
{
    if native_string_trap(number).is_some()
    {
        return unsafe {
            crate::toolbox::strings::dispatch(number, pointer, size, |n, p, s| host_gate(n, p, s))
        };
    }
    if !SESSION.with(|slot| slot.borrow().is_some())
    {
        return NOT_SUPPORTED as i64;
    }
    if let Some(result) = unsafe { worlds::dispatch(number, pointer, size) }
    {
        return result;
    }
    match number
    {
        POWER =>
        {
            SESSION.with(|session| {
                       let mut session = session.borrow_mut();
                       let Session { power,
                                     files,
                                     .. } = session.as_mut().unwrap();
                       let result=unsafe {request::<mac_abi::power::PowerCall>(pointer,size)}
                .and_then(|call|power.request(files,call));
                       result.err().unwrap_or(OK) as i64
                   })
        }
        MENU =>
        unsafe { menus::dispatch(pointer, size) },
        INSPECTION =>
        {
            SESSION.with(|session| {
                       let session = session.borrow();
                       let session = session.as_ref().unwrap();
                       let caller =
                           inspection::caller_identity(&session.inspection, files::current_owner());
                       if size < core::mem::size_of::<u32>()
                       {
                           return BAD_ARGUMENT as i64;
                       }
                       let operation =
                           match unsafe { request::<u32>(pointer, core::mem::size_of::<u32>()) }
                           {
                               Ok(operation) => *operation,
                               Err(error) => return error as i64
                           };
                       if matches!(operation,
                                   mac_abi::inspection::memory::BEGIN
                                   | mac_abi::inspection::memory::READ)
                       {
                           return match unsafe {
                                     request::<mac_abi::inspection::memory::Call>(pointer, size)
                                 }
                           {
                               Ok(call) =>
                               {
                                   inspection::memory(&session.inspection, caller, call) as i64
                               }
                               Err(error) => error as i64
                           };
                       }
                       match unsafe { request::<mac_abi::inspection::Call>(pointer, size) }
                       {
                           Ok(call) =>
                           {
                               if matches!(call.operation,
                                           mac_abi::inspection::KIND_BEGIN
                                           | mac_abi::inspection::KIND
                                           | mac_abi::inspection::KIND_ID)
                               {
                                   crate::toolbox::inspection::registered_kinds(&session.registry,
                                                                                caller.is_some(),
                                                                                call)
                                   as i64
                               }
                               else
                               {
                                   session.modules.inspect(&session.inspection,
                                                           caller.unwrap_or(files::current_owner()),
                                                           call)
                                   as i64
                               }
                           }
                           Err(error) => error as i64
                       }
                   })
        }
        THEME =>
        {
            match unsafe { request::<mac_abi::theme::Palette>(pointer, size) }
            {
                Ok(palette) =>
                {
                    *palette = crate::toolbox::theme::palette();
                    OK as i64
                }
                Err(error) => error as i64
            }
        }
        LOG =>
        {
            let call = match unsafe { request::<LogCall>(pointer, size) }
            {
                Ok(call) => call,
                Err(status) => return status as i64
            };
            if call.length > 1024 * 1024 || (call.length != 0 && call.bytes.is_null())
            {
                return BAD_ARGUMENT as i64;
            }
            let bytes = if call.length == 0
            {
                &[][..]
            }
            else
            {
                unsafe { core::slice::from_raw_parts(call.bytes, call.length) }
            };
            if core::str::from_utf8(bytes).is_err()
            {
                return BAD_ARGUMENT as i64;
            }
            use std::io::Write;
            match std::io::sink().write_all(bytes)
            {
                Ok(()) => OK as i64,
                Err(_) => IO_ERROR as i64
            }
        }
        DRAG =>
        unsafe {
            crate::toolbox::drag::native::dispatch(pointer, size, |call, input, output| {
                let owner = inspection::caller_ledger(files::current_owner())?;
                SESSION.with(|slot| {
                           let mut s = slot.borrow_mut();
                           let s = s.as_mut().unwrap();
                           drag::request(s, &owner, call, input, output)
                       })
            })
        },
        CLASSIC =>
        {
            let owner = match inspection::caller_ledger(files::current_owner())
            {
                Ok(owner) => owner,
                Err(status) => return status as i64
            };
            let call = match unsafe { request::<ClassicCall>(pointer, size) }
            {
                Ok(c) => c,
                Err(s) => return s as i64
            };
            if call.memory.is_null()
               || call.memory_length > 128 * 1024 * 1024
               || u64::from(call.guest_base) + call.memory_length as u64 > u64::from(u32::MAX) + 1
            {
                return BAD_ARGUMENT as i64;
            }
            let memory =
                unsafe { core::slice::from_raw_parts_mut(call.memory, call.memory_length) };
            SESSION.with(|slot| {
                       let mut s = slot.borrow_mut();
                       let s = s.as_mut().unwrap();
                       crate::toolbox::classic::dispatch(call.instruction,
                                                         &mut call.registers,
                                                         call.guest_base,
                                                         memory,
                                                         &mut drag::Services(s, &owner))
                       as i64
                   })
        }
        STANDARD_FILE =>
        {
            let ledger = match inspection::caller_ledger(files::current_owner())
            {
                Ok(ledger) => ledger,
                Err(status) => return status as i64
            };
            let call =
                unsafe { request::<mac_abi::standard_file::StandardFileCall>(pointer, size) };
            match call
            {
                Err(e) => e as i64,
                Ok(call) =>
                {
                    SESSION.with(|slot| {
                               let mut session = slot.borrow_mut();
                               let session = session.as_mut().unwrap();
                               session.standard_file
                                      .request(&ledger, call, &mut session.files)
                                      .err()
                                      .unwrap_or(OK) as i64
                           })
                }
            }
        }
        DIALOG =>
        {
            let call = match unsafe { request::<dialogs::DialogCall>(pointer, size) }
            {
                Ok(call) => call,
                Err(status) => return status as i64
            };
            if call.operation != dialogs::VALIDATE_ALERT
            {
                return NOT_SUPPORTED as i64;
            }
            SESSION.with(|slot| {
                       let session = slot.borrow();
                       let session = session.as_ref().unwrap();
                       let manager =
                           crate::toolbox::resources::manager::Manager::new(&session.resources,
                                                                            &session.application,
                                                                            session.fonts
                                                                                   .resources());
                       match crate::toolbox::dialogs::load(manager, call)
                       {
                           Ok(alert) =>
                           {
                               call.item = alert.items.len() as u16;
                               OK as i64
                           }
                           Err(status) => status as i64
                       }
                   })
        }
        CLOCK =>
        {
            match unsafe { request::<dates::ClockCall>(pointer, size) }
            {
                Ok(call) =>
                {
                    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                    {
                        Ok(time) =>
                        {
                            call.unix_seconds = time.as_secs() as i64;
                            call.microseconds = time.subsec_micros();
                            call.reserved = 0;
                            OK as i64
                        }
                        Err(_) => NOT_SUPPORTED as i64
                    }
                }
                Err(status) => status as i64
            }
        }
        FILE =>
        {
            let watch_operation = if size == size_of::<mac_abi::files::FileCall>()
            {
                match unsafe { request::<mac_abi::files::FileCall>(pointer, size) }
                {
                    Ok(call)
                        if matches!(call.operation,
                                    mac_abi::files::OPEN
                                    | mac_abi::files::WATCH_DIRECTORY
                                    | mac_abi::files::POLL_WATCH
                                    | mac_abi::files::DISPOSE_WATCH) =>
                    {
                        Some(call.operation)
                    }
                    Ok(_) => None,
                    Err(status) => return status as i64
                }
            }
            else
            {
                None
            };
            let write_admission = if size == size_of::<mac_abi::files::DataWriteCall>()
            {
                match unsafe { request::<mac_abi::files::DataWriteCall>(pointer, size) }
                {
                    Ok(call) => call.operation == mac_abi::files::BEGIN_DATA_WRITE,
                    Err(status) => return status as i64
                }
            }
            else
            {
                false
            };
            let stream_admission = if size == size_of::<mac_abi::files::ForkWriteCall>()
            {
                match unsafe { request::<mac_abi::files::ForkWriteCall>(pointer, size) }
                {
                    Ok(call) => call.operation == mac_abi::files::BEGIN_FORK_WRITE,
                    Err(status) => return status as i64
                }
            }
            else
            {
                false
            };
            let group_admission = if size == size_of::<mac_abi::files::IconGroupCall>()
            {
                match unsafe { request::<mac_abi::files::IconGroupCall>(pointer, size) }
                {
                    Ok(call) => call.operation == mac_abi::files::BEGIN_ICON_GROUP,
                    Err(status) => return status as i64
                }
            }
            else
            {
                false
            };
            let authority = if watch_operation.is_some()
                               || write_admission
                               || stream_admission
                               || group_admission
            {
                match inspection::caller_ledger(files::current_owner())
                {
                    Ok(owner) => Some(owner),
                    Err(status) => return status as i64
                }
            }
            else
            {
                None
            };
            SESSION.with(|slot| {
                       let mut session = slot.borrow_mut();
                       let session = session.as_mut().expect("session");
                       if watch_operation == Some(mac_abi::files::OPEN) {
                           if let Err(status) = session.files.prepare_forks((
                               session.registry.get(crate::toolbox::handles::KindCode(*b"FFRK")).unwrap(),
                               session.registry.get(crate::toolbox::handles::KindCode(*b"FFST")).unwrap())) { return status as i64; }
                       }
                       if write_admission {
                           if let Err(status)=session.files.prepare_writes((
                               session.registry.get(crate::toolbox::handles::KindCode(*b"FDWR")).unwrap(),
                               session.registry.get(crate::toolbox::handles::KindCode(*b"FDST")).unwrap())) {return status as i64;}
                       }
                       if stream_admission {
                           if let Err(status)=session.files.prepare_streams((
                               session.registry.get(crate::toolbox::handles::KindCode(*b"FSWR")).unwrap(),
                               session.registry.get(crate::toolbox::handles::KindCode(*b"FSST")).unwrap())) { return status as i64; }
                       }
                       if group_admission {
                           if let Err(status)=session.files.prepare_groups((
                               session.registry.get(crate::toolbox::handles::KindCode(*b"FIGR")).unwrap(),
                               session.registry.get(crate::toolbox::handles::KindCode(*b"FIGS")).unwrap())) {return status as i64;}
                       }
                       let watch = authority.map(|owner| crate::toolbox::files::Caller {
                           owner,
                           kinds: if watch_operation == Some(mac_abi::files::WATCH_DIRECTORY) {
                               Some((session.registry.get(crate::toolbox::handles::KindCode(*b"FWCH")).unwrap(),
                                     session.registry.get(crate::toolbox::handles::KindCode(*b"FWST")).unwrap()))
                           } else { None }
                       });
                       unsafe {
                           crate::toolbox::files::native::dispatch_owned(&mut session.files,
                                                                         files::current_owner(),
                                                                         pointer,
                                                                         size, watch.as_ref())
                       }
                   })
        }
        TEXTEDIT =>
        unsafe { textedit::dispatch(number, pointer, size) },
        POSITIONED_EVENT =>
        {
            SESSION.with(|slot| {
                       let mut session = slot.borrow_mut();
                       let s = session.as_mut().unwrap();
                       let drag_owner =
                           inspection::caller_identity(&s.inspection, files::current_owner());
                       let call =
                           match unsafe { request::<events::PositionedEventCall>(pointer, size) }
                           {
                               Ok(call) => call,
                               Err(status) => return status as i64
                           };
                       let ticks = (s.started.elapsed().as_nanos() * 60 / 1_000_000_000) as u64;
                       call.waited = 0;
                       // This scoped host harness has no Window Manager. It still uses the
                       // real native queue; coordinate/ownership assertions run in RV64.
                       match call.operation
                       {
                           events::POSITION_NEXT
                           | events::POSITION_WAIT
                           | events::POSITION_PEEK =>
                           {
                               let remove = call.operation != events::POSITION_PEEK;
                               let notice = if call.mask & (1 << EventKind::OsEvent as u16) != 0
                               {
                                   s.drag_notices.iter().position(|n| Some(n.owner) == drag_owner)
                               }
                               else
                               {
                                   None
                               };
                               let found = if let Some(index) = notice
                               {
                                   let event = s.drag_notices[index].event;
                                   if remove
                                   {
                                       s.drag_notices.remove(index);
                                   }
                                   Some((event, Default::default()))
                               }
                               else
                               {
                                   s.events.positioned_matching(call.mask, remove)
                               };
                               if let Some((event, position)) = found
                               {
                                   call.event = event;
                                   call.position = position;
                                   call.present = 1;
                               }
                               else
                               {
                                   if call.operation == events::POSITION_WAIT
                                      && call.timeout_ticks != 0
                                   {
                                       return NOT_SUPPORTED as i64;
                                   }
                                   call.event = s.events.null_event(ticks as u32);
                                   call.position = Default::default();
                                   call.present = 0;
                               }
                           }
                           events::POSITION_LOCATE =>
                           {
                               call.position = Default::default();
                               call.present = 1;
                           }
                           events::POSITION_INPUT =>
                           {
                               if call.window != 0
                               {
                                   return NOT_FOUND as i64;
                               }
                               call.event = s.events.null_event(ticks as u32);
                               call.position = Default::default();
                               call.present = 0;
                           }
                           _ => return BAD_ARGUMENT as i64
                       }
                       OK as i64
                   })
        }
        EVENTS | NEXT_EVENT | SCRAP =>
        {
            SESSION.with(|slot| {
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
                       if number == SCRAP
                       {
                           return unsafe {
                               crate::toolbox::scrap::native::dispatch(&mut session.scrap, number,
                                                                       pointer, size)
                           };
                       }
                       let ticks =
                           (session.started.elapsed().as_nanos() * 60 / 1_000_000_000) as u64;
                       let drag_owner =
                           inspection::caller_identity(&session.inspection, files::current_owner());
                       if number == NEXT_EVENT
                       {
                           return match unsafe { request::<NextEventCall>(pointer, size) }
                           {
                               Ok(call) =>
                               {
                                   if call.mask & (1 << EventKind::OsEvent as u16) != 0
                                      && let Some(index) =
                                          session.drag_notices
                                                 .iter()
                                                 .position(|n| Some(n.owner) == drag_owner)
                                   {
                                       call.event = session.drag_notices.remove(index).event;
                                       return OK as i64;
                                   }
                                   call.event = session.events
                                                       .next_matching(call.mask)
                                                       .unwrap_or_else(|| {
                                                           session.events.null_event(ticks as u32)
                                                       });
                                   OK as i64
                               }
                               Err(status) => status as i64
                           };
                       }
                       match unsafe { request::<events::EventCall>(pointer, size) }
                       {
                           Ok(call) =>
                           {
                               if call.operation == events::NATIVE_PEEK
                                  && call.mask & (1 << EventKind::OsEvent as u16) != 0
                                  && let Some(notice) =
                                      session.drag_notices
                                             .iter()
                                             .find(|n| Some(n.owner) == drag_owner)
                               {
                                   call.event = notice.event;
                                   call.present = 1;
                                   return OK as i64;
                               }
                               session.events.service(call, ticks as u32, ticks) as i64
                           }
                           Err(status) => status as i64
                       }
                   })
        }
        CRASH =>
        {
            let call = match unsafe { request::<mac_abi::crashes::CrashCall>(pointer, size) }
            {
                Ok(call) => call,
                Err(status) => return status as i64
            };
            if call.length > mac_abi::crashes::MAX_REPORT_BYTES
               || call.capacity > mac_abi::crashes::MAX_REPORT_BYTES
               || (call.length != 0 && call.data.is_null())
               || (call.capacity != 0 && call.output.is_null())
            {
                return BAD_ARGUMENT as i64;
            }
            let bytes = if call.length == 0
            {
                &[][..]
            }
            else
            {
                unsafe { core::slice::from_raw_parts(call.data, call.length) }
            };
            let Ok(input) = core::str::from_utf8(bytes)
            else
            {
                return BAD_ARGUMENT as i64;
            };
            let output = if call.capacity == 0
            {
                &mut [][..]
            }
            else
            {
                unsafe { core::slice::from_raw_parts_mut(call.output, call.capacity) }
            };
            SESSION.with(|slot| {
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
                       session.crashes
                              .request(call, input, output)
                              .map_or_else(|status| status, |_| OK) as i64
                   })
        }
        MEMORY =>
        {
            let owner = match inspection::caller_ledger(files::current_owner())
            {
                Ok(owner) => owner,
                Err(status) => return status as i64
            };
            SESSION.with(|slot| {
                       let Ok(mut session) = slot.try_borrow_mut()
                       else
                       {
                           return NOT_SUPPORTED as i64;
                       };
                       let Some(session) = session.as_mut()
                       else
                       {
                           return NOT_SUPPORTED as i64;
                       };
                       unsafe {
                           crate::toolbox::memory::native::dispatch(&mut session.memory, &owner,
                                                                    number, pointer, size)
                       }
                   })
        }

        CURSOR =>
        {
            let result =
                SESSION.with(|slot| {
                           let mut session = slot.borrow_mut();
                           let session = session.as_mut().unwrap();
                           let mouse = session.events.mouse;
                           if size == size_of::<quickdraw::CursorResourceCall>()
                           {
                               let call = unsafe {
                                   request::<quickdraw::CursorResourceCall>(pointer, size)
                               }?;
                               let resources =
                            crate::toolbox::resources::manager::Manager::new(&session.resources,
                                                                             &session.application,
                                                                             session.fonts
                                                                                    .resources());
                               let now = (session.started.elapsed().as_nanos() * 60 / 1_000_000_000)
                                         as u32;
                               session.cursors
                               .resource_request(call, mouse, now, |kind, id, scope| {
                                   let (_, resource) = resources.get(scope,
                                                                     ResourceKey { kind,
                                                                                   id })?;
                                   crate::toolbox::quickdraw::cursors::copy_resource(&resource.data)
                               })
                               .map(|_| ())
                           }
                           else
                           {
                               let call =
                                   unsafe { request::<quickdraw::CursorCall>(pointer, size) }?;
                               if call.color_count != 0
                                  && (call.colors.is_null()
                                      || call.color_count != 256
                                      || !(call.colors as usize).is_multiple_of(align_of::<u32>()))
                               {
                                   return Err(BAD_ARGUMENT);
                               }
                               let colors = if call.color_count == 0
                               {
                                   &[][..]
                               }
                               else
                               {
                                   unsafe {
                                       core::slice::from_raw_parts(call.colors, call.color_count)
                                   }
                               };
                               session.cursors.request(call, colors, mouse).map(|_| ())
                           }
                       });
            result.err().unwrap_or(OK) as i64
        }
        ICON =>
        {
            let result = SESSION.with(|slot| {
                let mut session = slot.borrow_mut();
                let session = session.as_mut().unwrap();
                let manager = crate::toolbox::resources::manager::Manager::new(
                    &session.resources, &session.application, session.fonts.resources());
                let (drawing, surface) = if size == size_of::<quickdraw::IconDataCall>() {
                    let call = unsafe { request::<quickdraw::IconDataCall>(pointer,size) }?;
                    (unsafe { crate::toolbox::quickdraw::native::icon_bytes(call) }?, call.surface)
                } else if size == size_of::<quickdraw::FileIconCall>()
                {
                    let call = unsafe { request::<quickdraw::FileIconCall>(pointer, size) }?;
                    if call.operation == quickdraw::ICON_LOAD_FILE
                    {
                        return crate::toolbox::quickdraw::icons_service::load_file(
                            &mut session.files, manager, call);
                    }
                    (crate::toolbox::quickdraw::icons_service::Drawing::file(
                        &mut session.files, manager, call)?, call.surface)
                }
                else
                {
                    let call = unsafe { request::<quickdraw::IconCall>(pointer, size) }?;
                    if call.operation != quickdraw::ICON_PLOT
                    {
                        return crate::toolbox::quickdraw::icons_service::query(manager, call);
                    }
                    (crate::toolbox::quickdraw::icons_service::Drawing::new(
                        manager, call, Default::default())?, call.surface)
                };
                if surface.is_null()
                {
                    return Err(NOT_SUPPORTED);
                }
                let surface = unsafe { request::<Surface>(surface.cast(), size_of::<Surface>()) }?;
                let mut canvas = unsafe { crate::toolbox::quickdraw::SurfaceExt::canvas(surface) }
                    .ok_or(BAD_ARGUMENT)?;
                drawing.paint(&mut canvas)
            });
            result.err().unwrap_or(OK) as i64
        }
        RESOURCE =>
        {
            if size == size_of::<resources::FileResourceCall>()
            {
                return SESSION.with(|slot| {
                           let mut session = slot.borrow_mut();
                           unsafe {
                               crate::toolbox::resources::inspect::dispatch(&mut session.as_mut()
                                                                                        .unwrap()
                                                                                        .files,
                                                                            files::current_owner(),
                                                                            pointer,
                                                                            size)
                           }
                       });
            }
            if size == size_of::<resources::FileVersionCall>()
            {
                return SESSION.with(|slot| {
                           let mut session = slot.borrow_mut();
                           unsafe {
                               crate::toolbox::resources::file::dispatch(&mut session.as_mut()
                                                                                     .unwrap()
                                                                                     .files,
                                                                         pointer,
                                                                         size)
                           }
                       });
            }
            SESSION.with(|slot| {
                       let session = slot.borrow();
                       let session = session.as_ref().unwrap();
                       let manager =
                           crate::toolbox::resources::manager::Manager::new(&session.resources,
                                                                            &session.application,
                                                                            session.fonts
                                                                                   .resources());
                       unsafe {
                           crate::toolbox::resources::native::dispatch(manager, pointer, size)
                       }
                   })
        }
        SCROLL_BOX =>
        {
            let ledger = match inspection::caller_ledger(files::current_owner())
            {
                Ok(ledger) => ledger,
                Err(status) => return status as i64
            };
            unsafe {
                crate::toolbox::controls::scrollbox_native::dispatch(pointer, size, |call| {
                    SESSION.with(|slot| {
                               let mut session = slot.borrow_mut();
                               let session = session.as_mut().unwrap();
                               if call.operation == scrollbox::CREATE && call.window != 0
                               {
                                   return Err(NOT_SUPPORTED);
                               }
                               session.scroll_boxes
                                      .request(&ledger,
                                               call,
                                               &session.fonts,
                                               crate::test_scrollbar::scrollbar_definition,
                                               host_gate)
                           })
                })
            }
        }
        CONTROL =>
        unsafe {
            let call = match request::<mac_abi::controls::ControlCall>(pointer, size)
            {
                Ok(c) => c,
                Err(e) => return e as i64
            };
            if call.operation >= mac_abi::controls::CREATE_STANDARD
            {
                let ledger = match inspection::caller_ledger(files::current_owner())
                {
                    Ok(ledger) => ledger,
                    Err(status) => return status as i64
                };
                return SESSION.with(|slot| {
                                  let session = slot.borrow();
                                  let Some(session) = session.as_ref()
                                  else
                                  {
                                      return NOT_SUPPORTED as i64;
                                  };
                                  let Ok(mut manager) = session.controls.try_borrow_mut()
                                  else
                                  {
                                      return SERVICE_BUSY as i64;
                                  };
                                  let owner = ledger.owner().incarnation();
                                  let result = (|| {
                                      let entry = match manager.definition(call, owner)?
                                      {
                                          0 => crate::test_cdef::standard_control_definition,
                                          1 => crate::test_scrollbar::scrollbar_definition,
                                          mac_abi::controls::CDEF_PROGRESS_BAR =>
                                          {
                                              crate::test_progress_bar::progress_bar_definition
                                          }
                                          mac_abi::controls::CDEF_STATIC_TEXT =>
                                          {
                                              crate::test_static_text::static_text_definition
                                          }
                                          _ => return Err(NOT_FOUND)
                                      };
                                      manager.request(&ledger, call, entry, host_gate).map(|_| ())
                                  })();
                                  result.map_or_else(|e| e as i64, |_| 0)
                              });
            }
            crate::toolbox::controls::native::dispatch(number,
                                                       pointer,
                                                       size,
                                                       |id| {
                                                           match id {
                                                               0 => Ok(crate::test_cdef::standard_control_definition),
                                                               1 => Ok(crate::test_scrollbar::scrollbar_definition),
                                                               mac_abi::controls::CDEF_PROGRESS_BAR => Ok(crate::test_progress_bar::progress_bar_definition),
                                                               mac_abi::controls::CDEF_STATIC_TEXT => Ok(crate::test_static_text::static_text_definition),
                                                               _ => Err(NOT_FOUND)
                                                           }
                                                       },
                                                       host_gate)
        },
        GEOMETRY =>
        {
            match unsafe { request::<GeometryCall>(pointer, size) }
            {
                Ok(call) => crate::toolbox::quickdraw::geometry::dispatch(call) as i64,
                Err(error) => error as i64
            }
        }
        SURFACE =>
        unsafe { crate::toolbox::quickdraw::surface_gate(number, pointer, size) },
        QUICKDRAW | FONT | REGION | COPY_BITS | MAPPING | IMAGE | PICTURE =>
        unsafe {
            SESSION.with(|slot| {
                       let session = slot.borrow();
                       crate::toolbox::quickdraw::native::dispatch(number,
                                                                   pointer,
                                                                   size,
                                                                   Some(&session.as_ref()
                                                                                .expect("session")
                                                                                .fonts))
                   })
        },
        _ => NOT_SUPPORTED as i64
    }
}

/// Mount actual sector bytes through firmware callbacks for public File Manager
/// tests. Context and manager are kept live together until the callback returns.
pub fn with_disk<R>(bytes: &[u8], run: impl FnOnce(&SystemServices) -> R) -> R
{
    with_disks(&[bytes], 0, run)
}
/// Mount several immutable sector sources and retain the actual startup disk
/// ordinal. All filesystem dispatch and volume identity comes from the service.
pub fn with_disks<R>(bytes: &[&[u8]], startup_disk: u32, run: impl FnOnce(&SystemServices) -> R)
                     -> R
{
    struct Disk<'a>(&'a [&'a [u8]]);
    unsafe extern "C" fn info(context: *mut (), index: u32, out: *mut DiskInfo) -> Status
    {
        let disks = unsafe { &*context.cast::<Disk<'_>>() };
        let Some(disk) = disks.0.get(index as usize)
        else
        {
            return NOT_FOUND;
        };
        unsafe {
            *out = DiskInfo { sector_count: disk.len() as u64 / 512,
                              read_only: 1,
                              ..Default::default() }
        };
        OK
    }
    unsafe extern "C" fn read(context: *mut (), index: u32, sector: u64, out: *mut u8) -> Status
    {
        let disks = unsafe { &*context.cast::<Disk<'_>>() };
        let Some(disk) = disks.0.get(index as usize)
        else
        {
            return NOT_FOUND;
        };
        let Some(start) = usize::try_from(sector).ok().and_then(|n| n.checked_mul(512))
        else
        {
            return BAD_ARGUMENT;
        };
        let Some(bytes) = start.checked_add(512).and_then(|end| disk.get(start..end))
        else
        {
            return BAD_ARGUMENT;
        };
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), out, 512) };
        OK
    }
    let mut disk = Disk(bytes);
    let boot = BootInfo { header: Header::for_type::<BootInfo>(),
                          ram_base: 0,
                          ram_size: 0,
                          log: None,
                          storage_context: core::ptr::from_mut(&mut disk).cast(),
                          disk_info: Some(info),
                          read_sector: Some(read),
                          write_sector: None,
                          flush_disk: None,
                          startup_disk,
                          flags: 0,
                          startup_partition_start: 0,
                          ram_regions: core::ptr::null(),
                          ram_region_count: 0,
                          reservations: core::ptr::null(),
                          reservation_count: 0,
                          display_scratch: Default::default(),
                          execution: core::ptr::null() };
    with_services(|services| {
        let storage =
            unsafe { crate::toolbox::files::BootStorage::from_boot_info(&boot) }.expect("storage");
        let files = crate::toolbox::files::Manager::new(storage).expect("fixture mount");
        SESSION.with(|slot| slot.borrow_mut().as_mut().unwrap().files = files);
        run(services)
    })
}

/// Decode real System/application resource forks into the scoped resident search
/// chain. The fixture builder is outside test bodies; SDK calls still use the
/// same native Resource Manager boundary as ECALL requests.
pub fn with_resource_forks<R>(system: &[u8], application: &[u8],
                              run: impl FnOnce(&SystemServices) -> R)
                              -> mac_formats::Result<R>
{
    let mut resources = mac_formats::resource::decode(system)?;
    let application = mac_formats::resource::decode(application)?;
    let fonts = crate::toolbox::quickdraw::fonts::FontManager::take_resources(&mut resources)
        .map_err(|status| match status {
            NO_MEMORY => mac_formats::Error::AllocationFailed("cannot allocate the fixture font catalog"),
            _ => mac_formats::Error::Malformed("cannot initialize the fixture font catalog")
        })?;
    Ok(with_services(|services| {
        SESSION.with(|slot| {
                   let mut session = slot.borrow_mut();
                   let session = session.as_mut().expect("session");
                   session.resources = resources;
                   session.application = application;
                   session.fonts = fonts;
               });
        run(services)
    }))
}

/// Faults exercise reported I/O failure/rollback; they never change manager results.
#[derive(Clone, Copy, Debug, Default)]
pub struct DiskFaults
{
    pub write_at:      Option<usize>,
    pub flush_at:      Option<usize>,
    pub partial_write: bool,
    pub persistent:    bool
}
#[derive(Clone, Copy, Debug, Default)]
pub struct DiskActivity
{
    pub reads:   usize,
    pub writes:  usize,
    pub flushes: usize
}
/// Mount an actual disposable host file, retaining storage and callbacks until
/// the scoped userspace session ends. Flush uses sync_all, not a RAM overlay.
pub fn with_file_disk<R>(path: &std::path::Path, read_only: bool, faults: DiskFaults,
                         run: impl FnOnce(&SystemServices) -> R)
                         -> std::io::Result<(R, DiskActivity)>
{
    with_file_disks(&[path], read_only, faults, run).map(|(value, mut activity)| {
                                                        (value, activity.remove(0))
                                                    })
}
/// Scope multiple actual writable images through the same storage callbacks.
pub fn with_file_disks<R>(paths: &[&std::path::Path], read_only: bool, faults: DiskFaults,
                          run: impl FnOnce(&SystemServices) -> R)
                          -> std::io::Result<(R, Vec<DiskActivity>)>
{
    use std::io::{Read, Seek, SeekFrom, Write};
    struct Disk
    {
        file:      std::fs::File,
        sectors:   u64,
        read_only: bool,
        faults:    DiskFaults,
        activity:  DiskActivity
    }
    struct Disks(Vec<Disk>);
    unsafe extern "C" fn info(context: *mut (), index: u32, output: *mut DiskInfo) -> Status
    {
        let disks = unsafe { &*context.cast::<Disks>() };
        let Some(d) = disks.0.get(index as usize)
        else
        {
            return NOT_FOUND;
        };
        unsafe {
            *output = DiskInfo { sector_count: d.sectors,
                                 read_only: u32::from(d.read_only),
                                 ..Default::default() };
        }
        OK
    }
    unsafe extern "C" fn read(context: *mut (), index: u32, sector: u64, output: *mut u8)
                              -> Status
    {
        let disks = unsafe { &mut *context.cast::<Disks>() };
        let Some(d) = disks.0.get_mut(index as usize)
        else
        {
            return NOT_FOUND;
        };
        if sector >= d.sectors
        {
            return BAD_ARGUMENT;
        }
        let output = unsafe { core::slice::from_raw_parts_mut(output, 512) };
        d.activity.reads += 1;
        d.file
         .seek(SeekFrom::Start(sector * 512))
         .and_then(|_| d.file.read_exact(output))
         .map_or(IO_ERROR, |_| OK)
    }
    unsafe extern "C" fn write(context: *mut (), index: u32, sector: u64, input: *const u8)
                               -> Status
    {
        let disks = unsafe { &mut *context.cast::<Disks>() };
        let Some(d) = disks.0.get_mut(index as usize)
        else
        {
            return NOT_FOUND;
        };
        if d.read_only
        {
            return READ_ONLY;
        }
        if sector >= d.sectors
        {
            return BAD_ARGUMENT;
        }
        let input = unsafe { core::slice::from_raw_parts(input, 512) };
        let attempt = d.activity.writes;
        d.activity.writes += 1;
        let fail = d.faults
                    .write_at
                    .is_some_and(|n| attempt == n || (d.faults.persistent && attempt >= n));
        if d.file.seek(SeekFrom::Start(sector * 512)).is_err()
        {
            return IO_ERROR;
        }
        if fail
        {
            if d.faults.partial_write
            {
                let _ = d.file.write_all(&input[..256]);
            }
            return IO_ERROR;
        }
        d.file.write_all(input).map_or(IO_ERROR, |_| OK)
    }
    unsafe extern "C" fn flush(context: *mut (), index: u32) -> Status
    {
        let disks = unsafe { &mut *context.cast::<Disks>() };
        let Some(d) = disks.0.get_mut(index as usize)
        else
        {
            return NOT_FOUND;
        };
        let attempt = d.activity.flushes;
        d.activity.flushes += 1;
        if d.faults.flush_at.is_some_and(|n| attempt == n || (d.faults.persistent && attempt >= n))
        {
            return IO_ERROR;
        }
        d.file.sync_all().map_or(IO_ERROR, |_| OK)
    }
    let mut disk = Disks(Vec::new());
    for path in paths
    {
        let file = std::fs::OpenOptions::new().read(true).write(!read_only).open(path)?;
        let sectors = file.metadata()?.len() / 512;
        disk.0.push(Disk { file,
                           sectors,
                           read_only,
                           faults,
                           activity: Default::default() });
    }
    let boot = BootInfo { header:                  Header::for_type::<BootInfo>(),
                          ram_base:                0,
                          ram_size:                0,
                          log:                     None,
                          storage_context:         core::ptr::from_mut(&mut disk).cast(),
                          disk_info:               Some(info),
                          read_sector:             Some(read),
                          write_sector:            Some(write),
                          flush_disk:              Some(flush),
                          startup_disk:            0,
                          flags:                   0,
                          startup_partition_start: 0,
                          ram_regions:             core::ptr::null(),
                          ram_region_count:        0,
                          reservations:            core::ptr::null(),
                          reservation_count:       0,
                          display_scratch:         Default::default(),
                          execution:               core::ptr::null() };
    let result = with_services(|services| {
        let storage=unsafe{crate::toolbox::files::BootStorage::from_boot_info(&boot)}.expect("file storage callbacks");
        let manager =
            crate::toolbox::files::Manager::new(storage).expect("disposable file volume mount");
        SESSION.with(|slot| slot.borrow_mut().as_mut().unwrap().files = manager);
        run(services)
    });
    Ok((result, disk.0.into_iter().map(|disk| disk.activity).collect()))
}

pub mod files;

/// Feed a physical dialog action into the same resident model used by System UI.
pub fn standard_file_action(action: crate::toolbox::standard_file::Action, when: u32)
                            -> Result<(), Status>
{
    let owner = inspection::caller_ledger(files::current_owner())?.owner().incarnation();
    SESSION.with(|slot| {
               let mut s = slot.borrow_mut();
               let s = s.as_mut().unwrap();
               let id = s.standard_file.active(owner).ok_or(NOT_FOUND)?;
               s.standard_file.action(id, &mut s.files, action, when)
           })
}
pub fn standard_file_entries() -> std::vec::Vec<mac_abi::files::CatalogInfo>
{
    let owner = inspection::caller_ledger(files::current_owner()).unwrap().owner().incarnation();
    SESSION.with(|slot| {
               let s = slot.borrow();
               let s = s.as_ref().unwrap();
               let id = s.standard_file.active(owner).unwrap();
               s.standard_file.get(id).unwrap().entries.clone()
           })
}

impl crate::toolbox::quickdraw::worlds::WorldHost for Session
{
    fn offscreen_worlds(&mut self) -> &mut crate::toolbox::quickdraw::worlds::Worlds
    {
        &mut self.worlds
    }
}

impl crate::toolbox::memory::lifetime::MemoryHost for Session
{
    fn release_memory(&mut self, ticket: mac_object_table::domain::ReleasedResource, storage: bool)
                      -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        self.memory.release(ticket, storage).map_err(Into::into)
    }
}

impl crate::toolbox::files::watch::lifetime::WatchHost for Session
{
    fn release_file_watch(&mut self, ticket: mac_object_table::domain::ReleasedResource,
                          storage: bool)
                          -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        crate::toolbox::files::watch::lifetime::WatchHost::release_file_watch(&mut self.files,
                                                                              ticket, storage)
    }
}

impl crate::toolbox::files::forks::lifetime::ForkHost for Session
{
    fn release_file_fork(&mut self, ticket: mac_object_table::domain::ReleasedResource,
                         storage: bool)
                         -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        crate::toolbox::files::forks::lifetime::ForkHost::release_file_fork(&mut self.files,
                                                                            ticket, storage)
    }
}

impl crate::toolbox::files::write::lifetime::WriteHost for Session
{
    fn release_file_write(&mut self, ticket: mac_object_table::domain::ReleasedResource,
                          storage: bool)
                          -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        crate::toolbox::files::write::lifetime::WriteHost::release_file_write(&mut self.files,
                                                                              ticket, storage)
    }
}

impl crate::toolbox::files::streams::lifetime::StreamHost for Session
{
    fn release_file_stream(&mut self, ticket: mac_object_table::domain::ReleasedResource,
                           storage: bool)
                           -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        crate::toolbox::files::streams::lifetime::StreamHost::release_file_stream(&mut self.files,
                                                                                  ticket, storage)
    }
}

impl crate::toolbox::files::groups::lifetime::GroupHost for Session
{
    fn release_file_group(&mut self, ticket: mac_object_table::domain::ReleasedResource,
                          storage: bool)
                          -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        crate::toolbox::files::groups::lifetime::GroupHost::release_file_group(&mut self.files,
                                                                               ticket, storage)
    }
}

impl crate::toolbox::files::pins::PinHost for Session
{
    fn release_file_pin(&mut self, ticket: mac_object_table::domain::ReleasedResource,
                        kind: crate::toolbox::files::pins::Registration)
                        -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        crate::toolbox::files::pins::PinHost::release_file_pin(&mut self.files, ticket, kind)
    }
}

impl crate::toolbox::handles::cleanup::context::Context for Session
{
    fn driver_registry(&mut self) -> Option<&mut crate::drivers::Registry>
    {
        Some(&mut self.modules)
    }
}
