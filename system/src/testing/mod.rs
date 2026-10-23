//! Host userspace conformance harness, available only under `test-support`.
//! Calls enter the same resident service dispatchers as native userspace; this
//! module supplies lifetime/scoping and the host adapter, never canned results.
pub mod shared;
pub mod menus;
pub mod scrap;
pub mod drag;
pub mod inspection;
use core::cell::RefCell;

use mac_abi::{traps::*, *};

struct Session
{
    inspection:    inspection::Domains,
    drag:          crate::toolbox::drag::Manager,
    drag_notices:  std::vec::Vec<crate::toolbox::drag::Notice>,
    standard_file: crate::toolbox::standard_file::Manager,
    scroll_boxes:  crate::toolbox::controls::scrollbox::Manager,
    // Definitions can query immutable font services while the Control Manager
    // is borrowed. Keep that manager's mutable borrow narrower than Session.
    controls:      RefCell<crate::toolbox::controls::objects::Manager>,
    crashes:       crate::toolbox::crashes::Manager,
    events:        crate::toolbox::events::EventManager,
    scrap:         crate::toolbox::scrap::Manager,
    textedit:      crate::toolbox::textedit::Manager,
    cursors:       crate::toolbox::quickdraw::cursors::Cursors,
    started:       std::time::Instant,
    memory:        crate::toolbox::memory::Manager,
    files:         crate::toolbox::files::Manager,
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
        let fonts = crate::toolbox::quickdraw::fonts::FontManager::take_resources(&mut resources);
        Self { inspection: inspection::Domains::new(),
               drag: Default::default(),
               drag_notices: std::vec::Vec::new(),
               controls: Default::default(),
               scroll_boxes: Default::default(),
               standard_file: Default::default(),
               crashes: Default::default(),
               events: crate::toolbox::events::EventManager::new(),
               scrap: Default::default(),
               textedit: Default::default(),
               cursors: Default::default(),
               started: std::time::Instant::now(),
               memory: Default::default(),
               files: Default::default(),
               fonts,
               resources,
               application: std::vec::Vec::new() }
    }
}
std::thread_local! {static SESSION:RefCell<Option<Session>>=const {RefCell::new(None)};}
struct Scope;
impl Drop for Scope
{
    fn drop(&mut self)
    {
        SESSION.with(|slot| {
                   slot.borrow_mut().take();
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
    match number
    {
        INSPECTION => SESSION.with(|session| {
            let session = session.borrow();
            let session = session.as_ref().unwrap();
            match unsafe { request::<mac_abi::inspection::Call>(pointer, size) }
            {
                Ok(call) => crate::toolbox::inspection::request(&session.inspection, files::current_owner(), call) as i64,
                Err(error) => error as i64
            }
        }),
        THEME => match unsafe { request::<mac_abi::theme::Palette>(pointer, size) }
        {
            Ok(palette) => { *palette = crate::toolbox::theme::palette(); OK as i64 },
            Err(error) => error as i64
        },
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
        DRAG => unsafe { crate::toolbox::drag::native::dispatch(pointer,size, |call,input,output| {
            SESSION.with(|slot| { let mut s=slot.borrow_mut(); let s=s.as_mut().unwrap();
                drag::request(s,call,input,output)
            })
        }) },
        CLASSIC => {
            let call = match unsafe { request::<ClassicCall>(pointer,size) } { Ok(c)=>c, Err(s)=>return s as i64 };
            if call.memory.is_null() || call.memory_length > 128*1024*1024
                || u64::from(call.guest_base) + call.memory_length as u64 > u64::from(u32::MAX)+1 { return BAD_ARGUMENT as i64; }
            let memory=unsafe { core::slice::from_raw_parts_mut(call.memory,call.memory_length) };
            SESSION.with(|slot| { let mut s=slot.borrow_mut(); let s=s.as_mut().unwrap();
                crate::toolbox::classic::dispatch(call.instruction,&mut call.registers,call.guest_base,memory,&mut drag::Services(s)) as i64
            })
        }
        STANDARD_FILE => {
            let call = unsafe { request::<mac_abi::standard_file::StandardFileCall>(pointer,size) };
            match call { Err(e)=>e as i64, Ok(call)=>SESSION.with(|slot| {
                let mut session=slot.borrow_mut();let session=session.as_mut().unwrap();
                session.standard_file.request(files::current_owner(),call,&mut session.files).err().unwrap_or(OK) as i64
            }) }
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
                            call.microseconds=time.subsec_micros();
                            call.reserved=0;
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
            SESSION.with(|slot| {
                       let mut session = slot.borrow_mut();
                       let session = session.as_mut().expect("session");
                       unsafe {
                           crate::toolbox::files::native::dispatch_owned(&mut session.files,
                                                                         files::current_owner(),
                                                                         pointer,
                                                                         size)
                       }
                   })
        }
        TEXTEDIT => SESSION.with(|slot| {
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
                        unsafe {
                            crate::toolbox::textedit::native::dispatch_with(number, pointer, size,
                        |request| match request {
                            crate::toolbox::textedit::native::Request::Edit(call, input, output) => crate::toolbox::textedit::clipboard::request(
                                &mut session.textedit, &mut session.scrap, call, input, output, &session.fonts),
                            crate::toolbox::textedit::native::Request::Display(call) => session.textedit.display_request(call),
                            crate::toolbox::textedit::native::Request::Font(call) => session.textedit.font_request(call, &session.fonts)
                        })
                        }
                    }),
        POSITIONED_EVENT => SESSION.with(|slot| {
            let mut session = slot.borrow_mut();
            let s = session.as_mut().unwrap();
            let call = match unsafe { request::<events::PositionedEventCall>(pointer, size) } { Ok(call) => call, Err(status) => return status as i64 };
            let ticks = (s.started.elapsed().as_nanos() * 60 / 1_000_000_000) as u64;
            call.waited = 0;
            // This scoped host harness has no Window Manager. It still uses the
            // real native queue; coordinate/ownership assertions run in RV64.
            match call.operation {
                events::POSITION_NEXT | events::POSITION_WAIT | events::POSITION_PEEK => {
                    let remove = call.operation != events::POSITION_PEEK;
                    let notice = if call.mask & (1 << EventKind::OsEvent as u16) != 0 {
                        s.drag_notices.iter().position(|n| n.owner == files::current_owner())
                    } else { None };
                    let found = if let Some(index) = notice {
                        let event = s.drag_notices[index].event;
                        if remove { s.drag_notices.remove(index); }
                        Some((event, Default::default()))
                    } else { s.events.positioned_matching(call.mask, remove) };
                    if let Some((event, position)) = found { call.event = event; call.position = position; call.present = 1; }
                    else {
                        if call.operation == events::POSITION_WAIT && call.timeout_ticks != 0 { return NOT_SUPPORTED as i64; }
                        call.event = s.events.null_event(ticks as u32); call.position = Default::default(); call.present = 0;
                    }
                }
                events::POSITION_LOCATE => { call.position = Default::default(); call.present = 1; }
                events::POSITION_INPUT => {
                    if call.window != 0 { return NOT_FOUND as i64; }
                    call.event = s.events.null_event(ticks as u32); call.position = Default::default(); call.present = 0;
                }
                _ => return BAD_ARGUMENT as i64
            }
            OK as i64
        }),
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
                       if number == NEXT_EVENT
                       {
                           return match unsafe { request::<NextEventCall>(pointer, size) }
                           {
                               Ok(call) =>
                               {
                                   if call.mask & (1 << EventKind::OsEvent as u16) != 0
                                      && let Some(index) = session.drag_notices.iter().position(|n| n.owner == files::current_owner()) {
                                        call.event = session.drag_notices.remove(index).event; return OK as i64;
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
                           Ok(call) => {
                               if call.operation == events::NATIVE_PEEK
                                  && call.mask & (1 << EventKind::OsEvent as u16) != 0
                                  && let Some(notice) = session.drag_notices.iter().find(|n| n.owner == files::current_owner())
                               {
                                   call.event = notice.event;
                                   call.present = 1;
                                   return OK as i64;
                               }
                               session.events.service(call, ticks as u32, ticks) as i64
                           },
                           Err(status) => status as i64
                       }
                   })
        }
        CRASH =>
        {
            let call = match unsafe { request::<mac_abi::crashes::CrashCall>(pointer, size) } {
                Ok(call) => call, Err(status) => return status as i64
            };
            if call.length > mac_abi::crashes::MAX_REPORT_BYTES
               || call.capacity > mac_abi::crashes::MAX_REPORT_BYTES
               || (call.length != 0 && call.data.is_null())
               || (call.capacity != 0 && call.output.is_null())
            { return BAD_ARGUMENT as i64; }
            let bytes = if call.length == 0 { &[][..] } else {
                unsafe { core::slice::from_raw_parts(call.data, call.length) }
            };
            let Ok(input) = core::str::from_utf8(bytes) else { return BAD_ARGUMENT as i64; };
            let output = if call.capacity == 0 { &mut [][..] } else {
                unsafe { core::slice::from_raw_parts_mut(call.output, call.capacity) }
            };
            SESSION.with(|slot| {
                let Ok(mut session) = slot.try_borrow_mut() else { return SERVICE_BUSY as i64; };
                let Some(session) = session.as_mut() else { return NOT_SUPPORTED as i64; };
                session.crashes.request(call, input, output).map_or_else(|status| status, |_| OK) as i64
            })
        }
        MEMORY =>
        {
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
                           crate::toolbox::memory::native::dispatch(&mut session.memory, number,
                                                                    pointer, size)
                       }
                   })
        }

        CURSOR =>
        {
            let result = SESSION.with(|slot| {
                let mut session = slot.borrow_mut();
                let session = session.as_mut().unwrap();
                let mouse = session.events.mouse;
                if size == size_of::<quickdraw::CursorResourceCall>()
                {
                    let call = unsafe { request::<quickdraw::CursorResourceCall>(pointer, size) }?;
                    let resources = crate::toolbox::resources::manager::Manager::new(
                        &session.resources, &session.application, session.fonts.resources());
                    let now = (session.started.elapsed().as_nanos() * 60 / 1_000_000_000) as u32;
                    session.cursors.resource_request(call, mouse, now, |kind, id, scope| {
                        resources.get(scope, ResourceKey { kind, id }).ok()
                            .map(|(_, resource)| resource.data.clone())
                    }).map(|_| ())
                }
                else
                {
                    let call = unsafe { request::<quickdraw::CursorCall>(pointer, size) }?;
                    if call.color_count != 0 && (call.colors.is_null() || call.color_count != 256
                        || !(call.colors as usize).is_multiple_of(align_of::<u32>()))
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    let colors = if call.color_count == 0 { &[][..] }
                    else { unsafe { core::slice::from_raw_parts(call.colors, call.color_count) } };
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
            if size == size_of::<resources::FileResourceCall>() {
                return SESSION.with(|slot| {
                    let mut session = slot.borrow_mut();
                    unsafe { crate::toolbox::resources::inspect::dispatch(&mut session.as_mut().unwrap().files, files::current_owner(), pointer, size) }
                });
            }
            if size == size_of::<resources::FileVersionCall>()
            {
                return SESSION.with(|slot| {
                    let mut session = slot.borrow_mut();
                    unsafe {crate::toolbox::resources::file::dispatch(&mut session.as_mut().unwrap().files,pointer,size)}
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
        SCROLL_BOX => unsafe {crate::toolbox::controls::scrollbox_native::dispatch(pointer,size,|call|SESSION.with(|slot|{
            let mut session=slot.borrow_mut();let session=session.as_mut().unwrap();
            if call.operation==scrollbox::CREATE && call.window!=0{return Err(NOT_SUPPORTED);}
            session.scroll_boxes.request(files::current_owner(),call,&session.fonts,crate::test_scrollbar::scrollbar_definition,host_gate)
        }))},
        CONTROL =>
        unsafe {
            let call = match request::<mac_abi::controls::ControlCall>(pointer, size) {
                Ok(c) => c, Err(e) => return e as i64,
            };
            if call.operation >= mac_abi::controls::CREATE_STANDARD {
                return SESSION.with(|slot| {
                    let session = slot.borrow();
                    let Some(session) = session.as_ref() else { return NOT_SUPPORTED as i64; };
                    let Ok(mut manager) = session.controls.try_borrow_mut() else { return SERVICE_BUSY as i64; };
                    let owner = files::current_owner();
                    let result = (|| {
                        let entry = match manager.definition(call, owner)? {
                            0 => crate::test_cdef::standard_control_definition,
                            1 => crate::test_scrollbar::scrollbar_definition,
                            mac_abi::controls::CDEF_PROGRESS_BAR => crate::test_progress_bar::progress_bar_definition,
                            mac_abi::controls::CDEF_STATIC_TEXT => crate::test_static_text::static_text_definition,
                            _ => return Err(NOT_FOUND)
                        };
                        manager.request(owner, call, entry, host_gate).map(|_| ())
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
        QUICKDRAW | FONT | REGION | COPY_BITS | MAPPING | IMAGE =>
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
                          startup_partition_start: 0 };
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
    let fonts = crate::toolbox::quickdraw::fonts::FontManager::take_resources(&mut resources);
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
    use std::io::{Read, Seek, SeekFrom, Write};
    struct Disk
    {
        file:      std::fs::File,
        sectors:   u64,
        read_only: bool,
        faults:    DiskFaults,
        activity:  DiskActivity
    }
    unsafe extern "C" fn info(context: *mut (), index: u32, output: *mut DiskInfo) -> Status
    {
        if index != 0
        {
            return NOT_FOUND;
        }
        let d = unsafe { &*context.cast::<Disk>() };
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
        let d = unsafe { &mut *context.cast::<Disk>() };
        if index != 0 || sector >= d.sectors
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
        let d = unsafe { &mut *context.cast::<Disk>() };
        if d.read_only
        {
            return READ_ONLY;
        }
        if index != 0 || sector >= d.sectors
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
        if index != 0
        {
            return NOT_FOUND;
        }
        let d = unsafe { &mut *context.cast::<Disk>() };
        let attempt = d.activity.flushes;
        d.activity.flushes += 1;
        if d.faults.flush_at.is_some_and(|n| attempt == n || (d.faults.persistent && attempt >= n))
        {
            return IO_ERROR;
        }
        d.file.sync_all().map_or(IO_ERROR, |_| OK)
    }
    let file = std::fs::OpenOptions::new().read(true).write(!read_only).open(path)?;
    let sectors = file.metadata()?.len() / 512;
    let mut disk = Disk { file,
                          sectors,
                          read_only,
                          faults,
                          activity: Default::default() };
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
                          startup_partition_start: 0 };
    let result = with_services(|services| {
        let storage=unsafe{crate::toolbox::files::BootStorage::from_boot_info(&boot)}.expect("file storage callbacks");
        let manager =
            crate::toolbox::files::Manager::new(storage).expect("disposable file volume mount");
        SESSION.with(|slot| slot.borrow_mut().as_mut().unwrap().files = manager);
        run(services)
    });
    Ok((result, disk.activity))
}

pub mod files;

/// Feed a physical dialog action into the same resident model used by System UI.
pub fn standard_file_action(action: crate::toolbox::standard_file::Action, when: u32)
                            -> Result<(), Status>
{
    SESSION.with(|slot| {
               let mut s = slot.borrow_mut();
               let s = s.as_mut().unwrap();
               let id = s.standard_file.active(files::current_owner()).ok_or(NOT_FOUND)?;
               s.standard_file.action(id, &mut s.files, action, when)
           })
}
pub fn standard_file_entries() -> std::vec::Vec<mac_abi::files::CatalogInfo>
{
    SESSION.with(|slot| {
               let s = slot.borrow();
               let s = s.as_ref().unwrap();
               let id = s.standard_file.active(files::current_owner()).unwrap();
               s.standard_file.get(id).unwrap().entries.clone()
           })
}
