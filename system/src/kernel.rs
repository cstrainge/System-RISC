//! Resident kernel entry, service boundary and startup orchestration.
use core::{alloc::Layout,
           fmt::{self, Write},
           panic::PanicInfo,
           ptr::{self, addr_of_mut}};

use linked_list_allocator::LockedHeap;
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
static HEAP: LockedHeap = LockedHeap::empty();
#[repr(C, align(16))]
struct HeapMemory([u8; 8 * 1024 * 1024]);
static mut HEAP_MEMORY: HeapMemory = HeapMemory([0; 8 * 1024 * 1024]);
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
        let mut regions: alloc::vec::Vec<_> = IMAGES.iter()
                                                    .filter_map(Once::get)
                                                    .filter(|i| i.limit > 0x83000000)
                                                    .map(|i| (i.base, i.limit))
                                                    .collect();
        regions.sort_unstable();
        let mut candidate = 0x83000000u64;
        for (start, end) in regions
        {
            if candidate + extent <= start
            {
                break;
            }
            candidate = candidate.max(end);
        }
        if candidate.checked_add(extent).is_none_or(|end| end > 0x87000000)
        {
            return Err(NO_MEMORY);
        }
        candidate
    };
    let limit = base + extent;
    let slot = IMAGES.iter().position(|i| i.get().is_none()).ok_or(NO_MEMORY)?;
    crate::loader::application_at(&resources, base).map_err(crate::loader::status)?;
    IMAGES[slot].call_once(|| {
                    AppImage { resources,
                               volume,
                               cnid,
                               creator,
                               file_type,
                               base,
                               limit }
                });
    Ok(slot)
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
                                                          image(index).map_or(&files.application
                                                                                  [..],
                                                                              |i| &i.resources),
                                                          fonts()?.resources()))
}

fn launch(info: &BootInfo) -> mac_formats::Result<()>
{
    let mut system = crate::loader::resources(&crate::loader::read_startup(info, "System")?)?;
    let finder = crate::loader::resources(&crate::loader::read_startup(info, "Finder")?)?;
    let finder_entry = crate::loader::application(&finder)?;
    let (base, limit) = crate::loader::reservation(&finder)?;
    IMAGES[0].call_once(|| {
                 AppImage { resources: finder.clone(),
                            volume: 0,
                            cnid: 0,
                            creator: *b"MACS",
                            file_type: *b"FNDR",
                            base,
                            limit }
             });
    if !crate::platform::setup_framebuffer()
    {
        return Err(mac_formats::Error("RAM framebuffer is unavailable"));
    }
    let fonts = crate::toolbox::quickdraw::fonts::FontManager::take_resources(&mut system);
    serial!("FONTS_READY: families={} resources loaded from System", fonts.catalog().len());
    // Immutable publication is acquire/release synchronized by Once. Runtime
    // readers never spin or take a mutable catalog guard.
    FONTS.call_once(|| fonts);
    let files = RESOURCE_FILES.call_once(|| {
                                  crate::toolbox::resources::manager::Files { system,
                                                                              application: finder }
                              });
    let definitions = crate::loader::Definitions::new(files);
    let runtime =
        crate::toolbox::runtime::Desktop::new(definitions).map_err(|_| {
                                              mac_formats::Error("Toolbox initialization failed")
                                          })?;
    DESKTOP.publish(runtime).map_err(|_| mac_formats::Error("desktop already published"))?;
    serial!("BOOT_SYSTEM_READY: services=0x{:x}", &SERVICES as *const SystemServices as usize);
    serial!("SYSTEM_FINDER_LAUNCH entry=0x{:x}", finder_entry);
    let thread=crate::machine::scheduler::spawn(1,0,finder_entry).map_err(|_|mac_formats::Error("Finder thread creation failed"))?;
    with_desktop(|desktop| desktop.started(1, thread)).map_err(|_| {
                                                          mac_formats::Error("Finder context \
                                                                              initialization \
                                                                              failed")
                                                      })?;
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
    unsafe {
        HEAP.lock().init(addr_of_mut!(HEAP_MEMORY).cast::<u8>(), 8 * 1024 * 1024);
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
    if info.read_startup.is_none()
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
    serial!("SYSTEM_ALLOCATION_FAILED: {} bytes", layout.size());
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
