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
static MEMORY: TryState<crate::toolbox::memory::Manager> = TryState::new();
// SAFETY: storage executes on hart0's fixed-owner service stack, with interrupts
// masked. The scheduler supplies genuine app thread identities; io::run hands
// execution to the bootstrap storage context before accessing opaque callbacks.
// They remain non-Send, including after application scheduling begins.
static FILES: OwnerState<crate::toolbox::files::Manager> =
    unsafe { OwnerState::new(crate::context::BOOT_THREAD, crate::context::current_thread) };
pub(crate) use crate::images::{Loaded as LoadedImage, Source as AppImage};
static IMAGES: TryState<crate::images::Manager> = TryState::new();

pub(crate) fn with_image<R>(binding: crate::images::Binding,
                            call: impl for<'a> FnOnce(&'a AppImage) -> R)
                            -> Option<R>
{
    let read = IMAGES.with(|images| images.read(binding)).ok()?.ok()?;
    Some(call(read.source()))
}
struct ImageCatalog<'a>
{
    primary: Option<&'a crate::images::Read>,
    extra:   Option<&'a crate::images::Read>
}
impl crate::toolbox::resources::manager::ImageCatalog for ImageCatalog<'_>
{
    fn resources(&self, id: u64) -> Option<&[mac_formats::resource::Resource]>
    {
        self.primary
            .into_iter()
            .chain(self.extra)
            .find(|read| read.binding().id() == id)
            .map(|read| &read.source().resources[..])
    }
}
pub(crate) fn load_application(owner: &crate::toolbox::handles::OwnerRef, creator: [u8; 4],
                               file_type: [u8; 4],
                               resources: alloc::vec::Vec<mac_formats::resource::Resource>)
                               -> Result<LoadedImage, Status>
{
    let authority = application_authority(&resources)?;
    admit_image(owner,
                AppImage { authority,
                           resources,
                           creator,
                           file_type })
}
fn admit_image(owner: &crate::toolbox::handles::OwnerRef, source: AppImage)
               -> Result<LoadedImage, Status>
{
    admit_shared_image(owner, alloc::sync::Arc::try_new(source).map_err(|_| NO_MEMORY)?)
}

pub(crate) fn copy_application(owner: &crate::toolbox::handles::OwnerRef,
                               original: crate::images::Binding)
                               -> Result<LoadedImage, Status>
{
    let source = IMAGES.with(|images| images.read(original).map(|read| read.shared_source()))
                       .map_err(AccessError::status)??;
    admit_shared_image(owner, source)
}

fn admit_shared_image(owner: &crate::toolbox::handles::OwnerRef,
                      source: alloc::sync::Arc<AppImage>)
                      -> Result<LoadedImage, Status>
{
    let (preferred, _) =
        crate::loader::reservation(&source.resources).map_err(crate::loader::status)?;
    let extent =
        crate::loader::application_extent(&source.resources).map_err(crate::loader::status)?;
    IMAGES.with(|images| {
              images.load(owner,
                          source,
                          preferred,
                          extent,
                          crate::machine::memory::APPLICATION_RELOCATION_BASE,
                          crate::machine::memory::IMAGE_LIMIT,
                          |resources, base| {
                              crate::loader::application_at(resources, base).map_err(|error| {
                                  serial!("APPLICATION_IMAGE_REFUSED: {error}");
                                  crate::loader::status(error)
                              })
                          })
          })
          .map_err(AccessError::status)?
}
pub(crate) fn release_image(ticket: mac_object_table::domain::ReleasedResource,
                            registration: crate::images::Registration)
                            -> Result<(), crate::toolbox::handles::cleanup::Failure>
{
    match IMAGES.with_owned(ticket, |ticket, images| images.release(ticket, registration))
    {
        Ok(result) => result,
        Err((_, ticket)) =>
        {
            Err(crate::toolbox::handles::cleanup::Failure::context_unavailable(ticket,
                                                                               "image manager"))
        }
    }
}
pub(crate) fn finish_image_origins(desktop: &mut crate::toolbox::runtime::Desktop)
{
    let mut previous = 0;
    while let Ok(Some((owner, origin))) = IMAGES.with(|images| images.next_retiring(previous))
    {
        previous = owner;
        if !crate::machine::scheduler::has_threads(owner)
        {
            desktop.evict_image_origin(origin);
        }
    }
    let _ = IMAGES.with(|images| images.finish_retiring());
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
pub(crate) fn poll_waiting_events() -> bool
{
    let Some(mut waiting) = crate::machine::scheduler::waiting()
    else
    {
        return false;
    };
    let context = crate::context::current_thread_context();
    let _scope = context.traps.enter();
    let Ok((foreground, fresh_input)) = DESKTOP.with(|desktop| {
                                            if crate::toolbox::inspection::take_membership_change()
                                            {
                                                desktop.notify_inspectors();
                                            }
                                            desktop.wake_waiters(&mut waiting)
                                        })
    else
    {
        return false;
    };
    for waiter in waiting.iter()
    {
        let app = &waiter.app;
        if waiter.ready
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
    fresh_input
}
pub(crate) fn with_files<R>(call: impl FnOnce(&mut crate::toolbox::files::Manager) -> R)
                            -> Result<R, AccessError>
{
    crate::machine::io::run(|| FILES.with(call))
}
/// A refused firmware handoff returns its unconsumed cleanup capability.
pub(crate) fn with_files_owned<A, R>(argument: A,
                                     call: impl FnOnce(A, &mut crate::toolbox::files::Manager) -> R)
                                     -> Result<R, (AccessError, A)>
{
    crate::machine::io::run(|| FILES.with_owned(argument, call))
}
pub(crate) fn with_memory<R>(call: impl FnOnce(&mut crate::toolbox::memory::Manager) -> R)
                             -> Result<R, AccessError>
{
    MEMORY.with(call)
}
pub(crate) fn with_memory_owned<A, R>(argument: A,
                                      call: impl FnOnce(A, &mut crate::toolbox::memory::Manager) -> R)
                                      -> Result<R, (AccessError, A)>
{
    MEMORY.with_owned(argument, call)
}
pub(crate) fn with_scrap<R>(call: impl FnOnce(&mut crate::toolbox::scrap::Manager) -> R)
                            -> Result<R, AccessError>
{
    SCRAP.with(call)
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
/// Authenticated source-selection metadata. Resource invocations retain a Read
/// for their complete borrow; carrying this receipt alone is not a lifetime pin.
#[derive(Clone, Copy)]
pub(crate) struct ResourceSelection
{
    owner: u64,
    image: Option<crate::images::Binding>
}
/// Capture source identity before an I/O handoff changes the executor. This is
/// selection metadata, not an image lease; invocation callers mask preemption.
pub(crate) fn current_resource_selection() -> ResourceSelection
{
    if let Some(selection) = crate::context::selected_resources()
    {
        return selection;
    }
    let context = crate::context::current_thread_context();
    let owner = context.identity().app.0;
    ResourceSelection { owner,
                        image: if owner == 0 { None } else { context.image() } }
}
pub(crate) fn with_resource_owner<R>(app: u64, call: impl FnOnce() -> R) -> Result<R, Status>
{
    let selected = crate::context::selected_resources();
    let index = match selected.filter(|selection| selection.owner == app)
    {
        Some(selection) => selection.image,
        None => Some(crate::machine::scheduler::image_for(app).ok_or(NOT_FOUND)?)
    };
    with_resource_binding(app, index, call)
}

/// Cleanup keeps resolving the subject's resources after its last thread is
/// gone. The binding can only be minted by the resident process directory.
/// This scope is synchronous under the interrupt-masked service boundary.
pub(crate) fn with_process_resources<R>(binding: &crate::toolbox::processes::ResourceBinding,
                                        call: impl FnOnce() -> R)
                                        -> Result<R, Status>
{
    with_resource_binding(binding.owner(),
                          if crate::images::Manager::root_only(binding.ledger())
                          {
                              None
                          }
                          else
                          {
                              binding.image()
                          },
                          call)
}

fn with_resource_binding<R>(app: u64, index: Option<crate::images::Binding>,
                            call: impl FnOnce() -> R)
                            -> Result<R, Status>
{
    let _read = match index
    {
        Some(binding) =>
        {
            Some(IMAGES.with(|images| images.read(binding)).map_err(AccessError::status)??)
        }
        None => None
    };
    crate::context::with_selected_resources(ResourceSelection { owner: app,
                                                                image: index },
                                            call).map_err(|error| {
                                                     serial!("RESOURCE_CONTEXT_REFUSED: {error}");
                                                     WRONG_THREAD
                                                 })
}
/// Image/catalog borrows end with this synchronous read. The higher-ranked
/// callback cannot smuggle resource references into a retained manager record.
pub(crate) fn with_resources<R>(call: impl for<'a> FnOnce(crate::toolbox::resources::manager::Manager<'a>)
                                               -> R)
                                -> Option<R>
{
    with_resources_in(mac_abi::resources::CHAIN, call).ok()
}

pub(crate) fn with_resources_in<R>(scope: mac_abi::resources::Scope,
                                   call: impl for<'a> FnOnce(crate::toolbox::resources::manager::Manager<'a>) -> R)
                                   -> Result<R, Status>
{
    if !scope.valid()
    {
        return Err(BAD_ARGUMENT);
    }
    let files = RESOURCE_FILES.get().ok_or(NOT_SUPPORTED)?;
    let selection = current_resource_selection();
    let needs_primary = scope != mac_abi::resources::SYSTEM;
    let primary = if needs_primary
    {
        match selection.image
        {
            Some(binding) =>
            {
                Some(IMAGES.with(|images| images.read(binding)).map_err(AccessError::status)??)
            }
            None => None
        }
    }
    else
    {
        None
    };
    let extra =
        match scope.image_id()
                   .filter(|id| primary.as_ref().is_none_or(|read| read.binding().id() != *id))
        {
            Some(id) =>
            {
                Some(IMAGES.with(|images| images.public_read(id))
                           .map_err(AccessError::status)??)
            }
            None => None
        };
    let catalog = ImageCatalog { primary: primary.as_ref(),
                                 extra:   extra.as_ref() };
    let application = primary.as_ref().map_or(&[][..], |read| &read.source().resources[..]);
    let origin = primary.as_ref().map_or(0, |read| read.binding().id());
    Ok(call(crate::toolbox::resources::manager::Manager::new(&files.system, application,
        fonts().ok_or(NOT_SUPPORTED)?.resources()).with_images(&catalog, origin, primary.as_ref())))
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
    let system = crate::loader::read_startup("System")?;
    let finder = crate::loader::read_startup("Finder")?;
    let mut processes = crate::toolbox::processes::Manager::new();
    let finder_id = processes.provision("Finder", 0, 0).map_err(|status| {
        serial!("FINDER_DOMAIN_REFUSED status={status}");
        mac_formats::Error::AllocationFailed("Finder lifetime domain admission failed")
    })?;
    serial!("BOOT_FINDER_DOMAIN app={finder_id}");
    let finder_owner =
        processes.ledger(finder_id)
                 .map_err(|_| mac_formats::Error::Malformed("Finder owner unavailable"))?;
    let kinds = crate::toolbox::handles::image_kinds().map_err(|_| mac_formats::Error::AllocationFailed("image kind admission failed"))?;
    IMAGES.publish(crate::images::Manager::new(crate::toolbox::handles::system_owner().map_err(|_| mac_formats::Error::AllocationFailed("System image owner unavailable"))?, kinds)
        .map_err(|_| mac_formats::Error::AllocationFailed("image manager allocation failed"))?)
        .map_err(|_| mac_formats::Error::Malformed("image manager already published"))?;
    let authority =
        policy_authority(&system, &finder).map_err(|_| {
                                              mac_formats::Error::Malformed("invalid Finder \
                                                                             authority policy")
                                          })?;
    let finder_loaded = admit_image(&finder_owner,
                                    AppImage { resources: finder,
                                               authority,
                                               creator: *b"MACS",
                                               file_type: *b"FNDR" }).map_err(|status| {
                            serial!("FINDER_IMAGE_REFUSED status={status}");
                            mac_formats::Error::Malformed("Finder image admission failed")
                        })?;
    processes.bind_image(finder_id, finder_loaded.binding())
             .map_err(|_| mac_formats::Error::Malformed("Finder image binding failed"))?;
    drop(finder_owner);
    processes.activate(finder_id)
             .map_err(|_| {
                 mac_formats::Error::Malformed("Finder lifetime domain could not become foreground")
             })?;
    let selection = {
        let binding = processes.resource_binding(finder_id).map_err(|_| {
            mac_formats::Error::Malformed("Finder bootstrap resource binding is unavailable")
        })?;
        ResourceSelection { owner: binding.owner(),
                            image: binding.image() }
    };
    crate::context::set_finder_app(finder_id);
    crate::context::with_bootstrap_resources(Some(selection), || {
        initialize_desktop(system, processes, finder_loaded)
    })?;
    let released = current_resource_selection();
    if released.owner != 0 || released.image.is_some()
    {
        return Err(mac_formats::Error::Malformed("bootstrap resource scope survived desktop \
                                                  initialization"));
    }
    serial!("BOOT_RESOURCE_SCOPE_RELEASED");
    // End the bootstrap resource scope and drop all admission guards before
    // leaving this stack permanently. Scheduled threads own their actual pins.
    crate::machine::scheduler::start()
}

fn initialize_desktop(mut system: alloc::vec::Vec<mac_formats::resource::Resource>,
                      processes: crate::toolbox::processes::Manager, finder_loaded: LoadedImage)
                      -> mac_formats::Result<()>
{
    let finder_entry = finder_loaded.entry();
    let finder_owner = processes.ledger(crate::context::finder_app().0).map_err(|_| {
        mac_formats::Error::Malformed("Finder bootstrap owner is unavailable")
    })?;
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
        crate::toolbox::runtime::Desktop::new(definitions, processes, finder_owner).map_err(|_| {
                                              mac_formats::Error::Malformed("Toolbox initialization failed")
                                          })?;
    boot_screen.completed(4)?;
    DESKTOP.publish(runtime)
           .map_err(|_| mac_formats::Error::Malformed("desktop already published"))?;
    boot_screen.completed(5)?;
    serial!("BOOT_SYSTEM_READY: services=0x{:x}", &SERVICES as *const SystemServices as usize);
    serial!("SYSTEM_FINDER_LAUNCH entry=0x{:x}", finder_entry);
    let thread =
        with_desktop(|desktop| {
            desktop.spawn_process(crate::context::finder_app().0, finder_loaded)
        }).map_err(|_| mac_formats::Error::Malformed("Finder owner directory is unavailable"))?
          .map_err(|status| {
              serial!("FINDER_THREAD_CREATION_FAILED status={status}");
              mac_formats::Error::Malformed("Finder thread creation failed")
          })?;
    boot_screen.completed(6)?;
    with_desktop(|desktop| desktop.started(crate::context::finder_app().0, thread)).map_err(|_| {
                                                          mac_formats::Error::Malformed("Finder context \
                                                                              initialization \
                                                                              failed")
                                                      })?;
    boot_screen.completed(7)?;
    drop(boot_screen);
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
    let memory = match unsafe { crate::machine::memory::validate_boot(info) }
    {
        Ok(memory) => memory,
        Err(error) =>
        {
            serial!("BOOT_MEMORY_REJECTED: {error}; rebuild the whole system and recreate stale \
                     boot media");
            return ptr::null();
        }
    };
    if let Err((kind, error)) = crate::machine::traps::validate_stack_configuration()
    {
        serial!("BOOT_STACK_REJECTED: {kind}: {error}; update the platform policy and rebuild");
        return ptr::null();
    }
    // SAFETY: first single-hart entry owns validated physical RAM. Graphical
    // and headless boot both supply the same real reservation contract; all
    // native execution needs owned pages, not an implicit headless fallback.
    if let Err(error) = unsafe { crate::machine::pages::initialize(memory) }
    {
        serial!("BOOT_MEMORY_REJECTED: {error}");
        return ptr::null();
    }
    let entry = match crate::machine::traps::prepare_entry()
    {
        Ok(entry) => entry,
        Err(error) =>
        {
            serial!("BOOT_SYSTEM_STACK_REJECTED: {error}");
            return ptr::null();
        }
    };
    entry.run(|| unsafe { start_services(info) })
}
/// # Safety
/// Entry validated the pinned BootInfo and switched onto the owned System stack.
unsafe fn start_services(info: &BootInfo) -> *const SystemServices
{
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
    let memory =
        match crate::toolbox::handles::memory_kinds().and_then(crate::toolbox::memory::Manager::new)
        {
            Ok(memory) => memory,
            Err(status) =>
            {
                serial!("MEMORY_MANAGER_INIT_FAILED status={status}");
                return ptr::null();
            }
        };
    if MEMORY.publish(memory).is_err()
    {
        return ptr::null();
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
        serial!("SYSTEM_STARTUP_FAILED: {}", error);
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
