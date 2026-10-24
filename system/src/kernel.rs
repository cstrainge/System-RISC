//! Resident kernel entry, service boundary and startup orchestration.
use core::{alloc::Layout,
           fmt::{self, Write},
           panic::PanicInfo,
           ptr::{self, addr_of_mut}};

use mac_abi::*;
use spin::Once;

use crate::state::{AccessError, OwnerState, TryState};

pub(crate) struct Serial;
impl Write for Serial
{
    fn write_str(&mut self, text: &str) -> fmt::Result
    {
        unsafe {
            log(text.as_ptr(), text.len());
        }
        Ok(())
    }
}
#[global_allocator]
static HEAP: crate::allocator::ResidentHeap = crate::allocator::ResidentHeap::empty();
#[repr(C, align(16))]
struct HeapMemory([u8; crate::machine::memory::SYSTEM_HEAP_BYTES]);
static mut HEAP_MEMORY: HeapMemory = HeapMemory([0; crate::machine::memory::SYSTEM_HEAP_BYTES]);
static BOOT_LOG: Once<Option<LogFn>> = Once::new();
static RESOURCE_FILES: Once<crate::toolbox::resources::manager::Files> = Once::new();
static FONTS: Once<crate::toolbox::quickdraw::fonts::FontManager> = Once::new();
static DESKTOP: TryState<crate::toolbox::runtime::Desktop> = TryState::new();
static SCRAP: TryState<crate::toolbox::scrap::Manager> = TryState::new();
static MEMORY: [TryState<crate::toolbox::memory::Manager>; 8] = [const { TryState::new() }; 8];
// SAFETY: storage executes on hart0's fixed-owner service stack, with interrupts
// masked. The scheduler supplies genuine app thread identities; io::run hands
// execution to the bootstrap storage context before accessing opaque callbacks.
// They remain non-Send, including after application scheduling begins.
static FILES: OwnerState<crate::toolbox::files::Manager> =
    unsafe { OwnerState::new(crate::context::BOOT_THREAD, crate::context::current_thread) };
pub(crate) struct AppImage
{
    pub authority: u64,
    pub resources: alloc::vec::Vec<mac_formats::resource::Resource>,
    pub volume:    u32,
    pub cnid:      u32,
    pub creator:   [u8; 4],
    pub file_type: [u8; 4],
    pub base:      u64,
    pub limit:     u64
}
static IMAGES: [Once<AppImage>; 8] = [const { Once::new() }; 8];
pub(crate) fn image(index: usize) -> Option<&'static AppImage>
{
    IMAGES.get(index)?.get()
}
pub(crate) fn cache_image(volume: u32, cnid: u32, creator: [u8; 4], file_type: [u8; 4],
                          resources: alloc::vec::Vec<mac_formats::resource::Resource>)
                          -> Result<usize, Status>
{
    if let Some((i, _)) = IMAGES.iter().enumerate().find(|(_, slot)| {
                                                       slot.get().is_some_and(|image| {
                                                                     image.volume == volume
                                                                     && image.cnid == cnid
                                                                     && image.creator == creator
                                                                     && image.file_type == file_type
                                                                     && image.resources == resources
                                                                 })
                                                   })
    {
        return Ok(i);
    }
    let (preferred, _) = crate::loader::reservation(&resources).map_err(crate::loader::status)?;
    let extent = crate::loader::application_extent(&resources).map_err(crate::loader::status)?;
    let overlaps = |base: u64| {
        IMAGES.iter().filter_map(Once::get).any(|i| base < i.limit && i.base < base + extent)
    };
    let base = if !overlaps(preferred)
    {
        preferred
    }
    else
    {
        let mut regions: alloc::vec::Vec<_> =
            IMAGES.iter()
                  .filter_map(Once::get)
                  .filter(|i| i.limit > crate::machine::memory::APPLICATION_RELOCATION_BASE)
                  .map(|i| (i.base, i.limit))
                  .collect();
        regions.sort_unstable();
        let mut candidate = crate::machine::memory::APPLICATION_RELOCATION_BASE;
        for (start, end) in regions
        {
            if candidate + extent <= start
            {
                break;
            }
            candidate = candidate.max(end);
        }
        if candidate.checked_add(extent).is_none_or(|end| end > crate::machine::memory::IMAGE_LIMIT)
        {
            return Err(NO_MEMORY);
        }
        candidate
    };
    let limit = base + extent;
    let slot = IMAGES.iter().position(|i| i.get().is_none()).ok_or(NO_MEMORY)?;
    let authority = application_authority(&resources)?;
    crate::loader::application_at(&resources, base).map_err(crate::loader::status)?;
    IMAGES[slot].call_once(|| {
                    AppImage { resources,
                               authority,
                               volume,
                               cnid,
                               creator,
                               file_type,
                               base,
                               limit }
                });
    Ok(slot)
}

fn application_authority(resources: &[mac_formats::resource::Resource]) -> Result<u64, Status>
{
    let Some(files) = RESOURCE_FILES.get()
    else
    {
        return Err(BAD_ABI);
    };
    policy_authority(&files.system, resources)
}

fn policy_authority(system: &[mac_formats::resource::Resource],
                    resources: &[mac_formats::resource::Resource])
                    -> Result<u64, Status>
{
    let Some(policy) =
        system.iter().find(|r| r.kind == mac_formats::authority::RESOURCE_KIND && r.id == 0)
    else
    {
        return Ok(0);
    };
    let identity = mac_formats::authority::image_digest(resources).map_err(crate::loader::status)?;
    mac_formats::authority::granted(&policy.data, identity).map_err(crate::loader::status)
}
pub(crate) fn services() -> *const SystemServices
{
    &SERVICES
}
static STARTED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

static SERVICES: SystemServices =
    SystemServices { header:       Header::for_type::<SystemServices>(),
                     trap:         crate::machine::traps::gateway,
                     capabilities: CAP_GRAPHICS | CAP_EVENT_WAIT | CAP_MONOTONIC_CLOCK };
static HEADLESS_SERVICES: SystemServices =
    SystemServices { header:       Header::for_type::<SystemServices>(),
                     trap:         crate::machine::traps::gateway,
                     capabilities: CAP_MONOTONIC_CLOCK };

pub(crate) unsafe extern "C" fn log(bytes: *const u8, length: usize) -> Status
{
    if bytes.is_null() && length != 0
    {
        return BAD_ARGUMENT;
    }
    match BOOT_LOG.get().copied().flatten()
    {
        Some(callback) =>
        unsafe { callback(bytes, length) },
        None => OK
    }
}
// Mutable state never exposes unchecked static references. These guards do not
// wait for a preempted holder. Stateful recursive calls return Busy; immutable
// resources/fonts and independent Surface services stay available during WDEFs.
pub(crate) fn with_desktop<R>(call: impl FnOnce(&mut crate::toolbox::runtime::Desktop) -> R)
                              -> Result<R, AccessError>
{
    DESKTOP.with(|desktop| {
               desktop.enter_caller();
               call(desktop)
           })
}

/// Timer/idle input pump. Never hold the scheduler lock while input routing may
/// invoke definitions or load an application. The desktop guard remains bounded
/// to this service, and no caller identity is borrowed across scheduling.
pub(crate) fn poll_waiting_events()
{
    let waiting = crate::machine::scheduler::waiting();
    let _scope = crate::context::current_thread_context().traps.enter();
    let Ok((ready, foreground)) = DESKTOP.with(|desktop| desktop.wake_waiters(&waiting))
    else
    {
        return;
    };
    for ((app, _), ready) in waiting.iter().zip(ready)
    {
        if ready
        {
            crate::trace::record(crate::trace::WAKE, *app, foreground, 0, 0, 0);
            if *app == foreground
            {
                crate::machine::scheduler::wake_interactive(*app);
            }
            else
            {
                crate::machine::scheduler::wake(*app);
            }
        }
    }
}
pub(crate) fn with_files<R>(call: impl FnOnce(&mut crate::toolbox::files::Manager) -> R)
                            -> Result<R, AccessError>
{
    crate::machine::io::run(|| FILES.with(call))
}
pub(crate) fn with_memory<R>(call: impl FnOnce(&mut crate::toolbox::memory::Manager) -> R)
                             -> Result<R, AccessError>
{
    MEMORY[crate::machine::scheduler::slot()].with(call)
}
pub(crate) fn with_scrap<R>(call: impl FnOnce(&mut crate::toolbox::scrap::Manager) -> R)
                            -> Result<R, AccessError>
{
    SCRAP.with(call)
}
pub(crate) fn release_memory()
{
    let _ = MEMORY[crate::machine::scheduler::slot()].with(|manager| manager.clear());
}
pub(crate) fn reload_image(index: usize) -> Result<usize, Status>
{
    let image = image(index).ok_or(NOT_FOUND)?;
    crate::loader::application_at(&image.resources, image.base).map_err(crate::loader::status)
}
pub(crate) fn fonts() -> Option<&'static crate::toolbox::quickdraw::fonts::FontManager>
{
    FONTS.get()
}
/// Reclaimable rendering caches must leave room for application loading and
/// thread stacks. Read under the allocator's own synchronization boundary.
pub(crate) fn heap_available() -> usize
{
    HEAP.available()
}
static RESOURCE_IMAGE: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(usize::MAX);
pub(crate) fn with_resource_owner<R>(app: u64, call: impl FnOnce() -> R) -> R
{
    use core::sync::atomic::Ordering::Relaxed;
    struct Restore(usize);
    impl Drop for Restore
    {
        fn drop(&mut self)
        {
            RESOURCE_IMAGE.store(self.0, core::sync::atomic::Ordering::Relaxed);
        }
    }
    let index = crate::machine::scheduler::image_for(app).unwrap_or(0);
    let _restore = Restore(RESOURCE_IMAGE.swap(index, Relaxed));
    call()
}
pub(crate) fn resources() -> Option<crate::toolbox::resources::manager::Manager<'static>>
{
    let files = RESOURCE_FILES.get()?;
    let override_image = RESOURCE_IMAGE.load(core::sync::atomic::Ordering::Relaxed);
    let index = if override_image == usize::MAX
    {
        crate::machine::scheduler::image()
    }
    else
    {
        override_image
    };
    Some(crate::toolbox::resources::manager::Manager::new(&files.system,
                                                          &image(index)?.resources,
                                                          fonts()?.resources()))
}

fn launch(info: &BootInfo) -> mac_formats::Result<()>
{
    let ram_end = info.ram_base
                      .checked_add(info.ram_size)
                      .ok_or(mac_formats::Error::Malformed("RAM range overflow"))?;
    let module_arena = crate::machine::memory::module_arena(ram_end).map_err(|_| {
                           mac_formats::Error::Malformed("module RAM reservation unavailable")
                       })?;
    crate::drivers::initialize(module_arena,crate::machine::MODULE_TARGET).map_err(|_|mac_formats::Error::Malformed("module runtime initialization failed"))?;
    crate::machine::driver_devices::initialize().map_err(|_| {
                                                    mac_formats::Error::Malformed("device service \
                                                                                   initialization \
                                                                                   failed")
                                                })?;
    crate::drivers::file_sources::runtime::initialize().map_err(|_| mac_formats::Error::Malformed("backing-file service initialization failed"))?;
    let mut system = crate::loader::read_startup("System")?;
    let finder = crate::loader::read_startup("Finder")?;
    let finder_entry = crate::loader::application(&finder)?;
    let (base, limit) = crate::loader::reservation(&finder)?;
    let authority = policy_authority(&system, &finder).map_err(|_| {
                        mac_formats::Error::Malformed("invalid application authority policy while \
                                                       admitting Finder")
                    })?;
    IMAGES[0].call_once(|| {
                 AppImage { resources: finder,
                            authority,
                            volume: 0,
                            cnid: 0,
                            creator: *b"MACS",
                            file_type: *b"FNDR",
                            base,
                            limit }
             });
    let fonts = crate::toolbox::quickdraw::fonts::FontManager::take_resources(&mut system);
    serial!("FONTS_READY: families={} resources loaded from System", fonts.catalog().len());
    // Progress uses the real loaded CDEF before any application loop exists.
    let fonts = FONTS.call_once(|| fonts);
    let files = RESOURCE_FILES.call_once(|| crate::toolbox::resources::manager::Files { system });
    let mut definitions = crate::loader::Definitions::new(files);
    let mut boot_screen =
        crate::boot_screen::BootScreen::begin(&files.system, fonts, &mut definitions)?;
    if !crate::platform::setup_framebuffer()
    {
        return Err(mac_formats::Error::Malformed("RAM framebuffer is unavailable"));
    }
    serial!("BOOT_PANEL_READY stages=7");
    // Immutable publication is acquire/release synchronized by Once. Runtime
    // readers never spin or take a mutable catalog guard.
    boot_screen.completed(1)?;
    boot_screen.completed(2)?;
    crate::toolbox::files::types::initialize().map_err(|status| {
                                                  serial!("FILE_TYPES_ERROR status={}", status);
                                                  mac_formats::Error::Malformed("File Types is \
                                                                                 invalid or \
                                                                                 unreadable")
                                              })?;
    // Module-owned icon families resolve the System palettes at admission.
    // Publish immutable resource/font services before entering any driver;
    // exports still mount before Finder or another application can run.
    crate::drivers::discover().map_err(|_| {
                                  mac_formats::Error::Malformed("module discovery failed")
                              })?;
    if let Err(status) = crate::toolbox::files::mount_startup_exports()
    {
        serial!("HOST_VOLUME_DISCOVERY_FAILED status={}", status);
    }
    boot_screen.completed(3)?;
    let runtime =
        crate::toolbox::runtime::Desktop::new(definitions).map_err(|_| {
                                              mac_formats::Error::Malformed("Toolbox initialization failed")
                                          })?;
    boot_screen.completed(4)?;
    DESKTOP.publish(runtime)
           .map_err(|_| mac_formats::Error::Malformed("desktop already published"))?;
    boot_screen.completed(5)?;
    serial!("BOOT_SYSTEM_READY: services=0x{:x}", &SERVICES as *const SystemServices as usize);
    serial!("SYSTEM_FINDER_LAUNCH entry=0x{:x}", finder_entry);
    let thread=crate::machine::scheduler::spawn(crate::context::finder_app().0,0,finder_entry).map_err(|_|mac_formats::Error::Malformed("Finder thread creation failed"))?;
    boot_screen.completed(6)?;
    with_desktop(|desktop| desktop.started(crate::context::finder_app().0, thread)).map_err(|_| {
                                                          mac_formats::Error::Malformed("Finder context \
                                                                              initialization \
                                                                              failed")
                                                      })?;
    boot_screen.completed(7)?;
    drop(boot_screen);
    crate::machine::scheduler::start()
}

/// # Safety
/// Bootstrap supplies a valid header and compatible BootInfo for this call.
/// Startup callbacks/context remain valid while System or Finder is running.
pub unsafe extern "C" fn start(info: *const BootInfo) -> *const SystemServices
{
    if info.is_null()
    {
        return ptr::null();
    }
    let header = unsafe { &*info.cast::<Header>() };
    if !header.supports::<BootInfo>()
    {
        return ptr::null();
    }
    let info = unsafe { &*info };
    if info.flags & !mac_abi::BOOT_GRAPHICAL != 0
    {
        return ptr::null();
    }
    use core::sync::atomic::Ordering;
    // Secondary harts and repeated entry cannot replace live manager state or
    // race allocator initialization. Failed initialization is not retried over
    // partially published state; reset is the only supported bootstrap retry.
    if crate::context::bootstrap_thread() != crate::context::BOOT_THREAD
       || STARTED.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err()
    {
        return ptr::null();
    }
    BOOT_LOG.call_once(|| info.log);
    if let Err(error) = unsafe { crate::machine::memory::validate_boot(info) }
    {
        serial!("BOOT_MEMORY_REJECTED: {error}; rebuild the whole system and recreate stale boot \
                 media");
        return ptr::null();
    }
    unsafe {
        HEAP.initialize(addr_of_mut!(HEAP_MEMORY).cast::<u8>(),
                        crate::machine::memory::SYSTEM_HEAP_BYTES);
    }
    crate::machine::traps::install();
    let files = match unsafe { crate::toolbox::files::BootStorage::from_boot_info(info) }
    {
        Some(storage) =>
        {
            match crate::toolbox::files::Manager::new(storage)
            {
                Ok(files) => files,
                Err(status) =>
                {
                    serial!("FILE_MANAGER_INIT_FAILED status={}", status);
                    return ptr::null();
                }
            }
        }
        None => crate::toolbox::files::Manager::default()
    };
    for memory in &MEMORY
    {
        let _ = memory.publish(Default::default());
    }
    if FILES.publish(files).is_err()
       || SCRAP.publish(crate::toolbox::scrap::Manager::default()).is_err()
    {
        return ptr::null();
    }
    serial!("System: native service table ready");
    if info.flags & mac_abi::BOOT_GRAPHICAL == 0
    {
        return &HEADLESS_SERVICES;
    }
    if let Err(error) = launch(info)
    {
        serial!("FINDER_LOAD_FAILED: {}", error);
        return ptr::null();
    }
    &SERVICES
}

pub fn allocation_failed(layout: Layout) -> !
{
    let free = heap_available();
    serial!("SYSTEM_ALLOCATION_FAILED: {} bytes, {} free", layout.size(), free);
    halt()
}
pub fn panic(info: &PanicInfo<'_>) -> !
{
    serial!("SYSTEM_PANIC: {}", info);
    halt()
}
fn halt() -> !
{
    loop
    {
        core::hint::spin_loop();
    }
}
