//! Host userspace conformance harness, available only under `test-support`.
//! Calls enter the same resident service dispatchers as native userspace; this
//! module supplies lifetime/scoping and the host adapter, never canned results.
pub mod shared;
pub mod menus;
pub mod scrap;
use core::cell::RefCell;

use mac_abi::{traps::*, *};

struct Session
{
    events:      crate::toolbox::events::EventManager,
    scrap:       crate::toolbox::scrap::Manager,
    textedit:    crate::toolbox::textedit::Manager,
    started:     std::time::Instant,
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
        Self { events: crate::toolbox::events::EventManager::new(),
               scrap: Default::default(),
               textedit: Default::default(),
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
                           crate::toolbox::files::native::dispatch(&mut session.files, pointer,
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
                        |call, input, output| crate::toolbox::textedit::clipboard::request(
                            &mut session.textedit, &mut session.scrap, call, input, output))
                        }
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
                           Ok(call) => session.events.service(call, ticks as u32, ticks) as i64,
                           Err(status) => status as i64
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
                          write_sector:            None,
                          flush_disk:              None,
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
                          startup_context:         core::ptr::null_mut(),
                          read_startup:            None,
                          storage_context:         core::ptr::from_mut(&mut disk).cast(),
                          disk_info:               Some(info),
                          read_sector:             Some(read),
                          write_sector:            Some(write),
                          flush_disk:              Some(flush),
                          startup_disk:            0,
                          reserved:                0,
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
