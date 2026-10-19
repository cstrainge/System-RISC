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
static MEMORY: TryState<crate::toolbox::memory::Manager> = TryState::new();
// SAFETY: firmware runs only bootstrap OS thread1 on hart0. Before scheduling
// starts, this provider must be replaced by the scheduler's genuine ThreadId.
// Opaque firmware callbacks remain on the I/O owner; no Send impl is invented.
static FILES: OwnerState<crate::toolbox::files::Manager> =
    unsafe { OwnerState::new(crate::context::BOOT_THREAD, crate::context::bootstrap_thread) };
static STARTED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

static SERVICES: SystemServices = SystemServices { header:       Header::for_type::<SystemServices>(),
                                                   trap:         crate::machine::traps::gateway,
                                                   capabilities: CAP_GRAPHICS };
static HEADLESS_SERVICES: SystemServices =
    SystemServices { header:       Header::for_type::<SystemServices>(),
                     trap:         crate::machine::traps::gateway,
                     capabilities: 0 };

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
    DESKTOP.with(call)
}
pub(crate) fn with_files<R>(call: impl FnOnce(&mut crate::toolbox::files::Manager) -> R)
                            -> Result<R, AccessError>
{
    FILES.with(call)
}
pub(crate) fn with_memory<R>(call: impl FnOnce(&mut crate::toolbox::memory::Manager) -> R)
                             -> Result<R, AccessError>
{
    MEMORY.with(call)
}
pub(crate) fn fonts() -> Option<&'static crate::toolbox::quickdraw::fonts::FontManager>
{
    FONTS.get()
}
pub(crate) fn resources() -> Option<crate::toolbox::resources::manager::Manager<'static>>
{
    let files = RESOURCE_FILES.get()?;
    Some(crate::toolbox::resources::manager::Manager::new(&files.system,
                                                          &files.application,
                                                          fonts()?.resources()))
}

fn launch(info: &BootInfo) -> mac_formats::Result<()>
{
    let mut system = crate::loader::resources(&crate::loader::read_startup(info, "System")?)?;
    let finder = crate::loader::resources(&crate::loader::read_startup(info, "Finder")?)?;
    let finder_entry = crate::loader::application(&finder)?;
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
    let entry: ApplicationEntry = unsafe { core::mem::transmute(finder_entry) };
    let status = unsafe { entry(&SERVICES) };
    serial!("FINDER_EXIT status={}", status);
    Ok(())
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
    if FILES.publish(files).is_err()
       || MEMORY.publish(crate::toolbox::memory::Manager::default()).is_err()
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
