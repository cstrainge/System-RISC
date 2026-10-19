//! Host userspace conformance harness, available only under `test-support`.
//! Calls enter the same resident service dispatchers as native userspace; this
//! module supplies lifetime/scoping and the host adapter, never canned results.
pub mod shared;
use core::cell::RefCell;

use mac_abi::{traps::*, *};

struct Session
{
    memory:      crate::toolbox::memory::Manager,
    files:       crate::toolbox::files::Manager,
    fonts:       crate::toolbox::quickdraw::fonts::FontManager,
    resources:   std::vec::Vec<mac_formats::resource::Resource>,
    application: std::vec::Vec<mac_formats::resource::Resource>
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
        Self { memory: Default::default(),
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
        FILE =>
        {
            SESSION.with(|slot| {
                       let mut session = slot.borrow_mut();
                       let session = session.as_mut().expect("session");
                       unsafe {
                           crate::toolbox::files::native::dispatch(&mut session.files, pointer,
                                                                   size)
                       }
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

        RESOURCE =>
        {
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
        CONTROL =>
        unsafe {
            crate::toolbox::controls::native::dispatch(number,
                                                       pointer,
                                                       size,
                                                       |id| {
                                                           match id {0=>Ok(crate::test_cdef::standard_control_definition),1=>Ok(crate::test_scrollbar::scrollbar_definition),_=>Err(NOT_FOUND)}
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
    struct Disk<'a>(&'a [u8]);
    unsafe extern "C" fn info(context: *mut (), index: u32, out: *mut DiskInfo) -> Status
    {
        if index != 0
        {
            return NOT_FOUND;
        }
        let disk = unsafe { &*context.cast::<Disk<'_>>() };
        unsafe {
            *out = DiskInfo { sector_count: disk.0.len() as u64 / 512,
                              read_only: 1,
                              ..Default::default() }
        };
        OK
    }
    unsafe extern "C" fn read(context: *mut (), index: u32, sector: u64, out: *mut u8) -> Status
    {
        if index != 0
        {
            return NOT_FOUND;
        }
        let disk = unsafe { &*context.cast::<Disk<'_>>() };
        let Some(start) = usize::try_from(sector).ok().and_then(|n| n.checked_mul(512))
        else
        {
            return BAD_ARGUMENT;
        };
        let Some(bytes) = start.checked_add(512).and_then(|end| disk.0.get(start..end))
        else
        {
            return BAD_ARGUMENT;
        };
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), out, 512) };
        OK
    }
    let mut disk = Disk(bytes);
    let boot = BootInfo { header:                  Header::for_type::<BootInfo>(),
                          ram_base:                0,
                          ram_size:                0,
                          log:                     None,
                          startup_context:         core::ptr::null_mut(),
                          read_startup:            None,
                          storage_context:         core::ptr::from_mut(&mut disk).cast(),
                          disk_info:               Some(info),
                          read_sector:             Some(read),
                          startup_disk:            0,
                          reserved:                0,
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
