//! Resident kernel entry, service boundary and startup orchestration.
use core::{alloc::Layout,
           fmt::{self, Write},
           panic::PanicInfo,
           ptr};

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
type ApplicationImageError = crate::images::LoadError<crate::loader::LoadError>;

fn application_image_status(error: ApplicationImageError) -> Status
{
    match error
    {
        crate::images::LoadError::Admission(status) => status,
        crate::images::LoadError::Write(cause) => crate::loader::status(cause)
    }
}

pub(crate) fn load_application(owner: &crate::toolbox::handles::OwnerRef, creator: [u8; 4],
                               file_type: [u8; 4],
                               resources: alloc::vec::Vec<mac_formats::resource::Resource>)
                               -> Result<LoadedImage, Status>
{
    let authority = application_authority(&resources)?;
    admit_image(owner,
                AppImage { role: crate::images::Role::Application { authority },
                           resources,
                           creator,
                           file_type }).map_err(application_image_status)
}
fn admit_image(owner: &crate::toolbox::handles::OwnerRef, source: AppImage)
               -> Result<LoadedImage, ApplicationImageError>
{
    // Immutable package sources are interned by the System image registry.
    // Their resource backing is attributed at the storage read, before this move.
    let source = crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
                     alloc::sync::Arc::try_new(source)
                 }).map_err(|_| NO_MEMORY)?;
    admit_shared_image(owner, source)
}

pub(crate) fn copy_application(owner: &crate::toolbox::handles::OwnerRef,
                               original: crate::images::Binding)
                               -> Result<LoadedImage, Status>
{
    let source = IMAGES.with(|images| images.read(original).map(|read| read.shared_source()))
                       .map_err(AccessError::status)??;
    admit_shared_image(owner, source).map_err(application_image_status)
}

fn admit_shared_image(owner: &crate::toolbox::handles::OwnerRef,
                      source: alloc::sync::Arc<AppImage>)
                      -> Result<LoadedImage, ApplicationImageError>
{
    if !matches!(source.role, crate::images::Role::Application { .. })
    {
        return Err(crate::images::LoadError::Admission(ACCESS_DENIED));
    }
    let (preferred, _) =
        crate::loader::reservation(&source.resources)
            .map_err(|cause| crate::images::LoadError::Write(cause.into()))?;
    let extent =
        crate::loader::application_extent(&source.resources)
            .map_err(|cause| crate::images::LoadError::Write(cause.into()))?;
    IMAGES.with(|images| {
              images.load(owner,
                          source,
                          preferred,
                          extent,
                          crate::machine::memory::APPLICATION_RELOCATION_BASE,
                          crate::machine::memory::IMAGE_LIMIT,
                          alloc::vec::Vec::new(),
                          |resources, base| {
                              crate::loader::application_at(resources, base).map_err(|error| {
                                  serial!("APPLICATION_IMAGE_REFUSED: {error}");
                                  error
                              })
                          })
          })
          .map_err(|cause| crate::images::LoadError::Admission(cause.status()))?
}
/// Module packages use the same immutable source and owner-alias registry as
/// applications, with their own role and the exact permanent module RAM range.
pub(crate) fn load_module(owner: &crate::toolbox::handles::OwnerRef,
                          source: alloc::sync::Arc<AppImage>, base: u64, extent: u64,
                          executable: alloc::vec::Vec<core::ops::Range<u64>>,
                          write: impl FnOnce(&[mac_formats::resource::Resource],
                                 u64)
                                 -> Result<usize, Status>)
                          -> Result<LoadedImage, Status>
{
    if source.role != crate::images::Role::Module
       || owner.owner().kind() != mac_object_table::domain::DomainKind::Module
    {
        return Err(ACCESS_DENIED);
    }
    IMAGES.with(|images| {
              images.load(owner,
                          source,
                          base,
                          extent,
                          base,
                          base.checked_add(extent).ok_or(NO_MEMORY)?,
                          executable,
                          write)
          })
          .map_err(AccessError::status)?
          .map_err(|cause| {
              match cause
              {
                  crate::images::LoadError::Admission(status)
                  | crate::images::LoadError::Write(status) => status
              }
          })
}
pub(crate) fn module_execution(binding: crate::images::Binding)
                               -> Result<crate::images::ModuleExecution, Status>
{
    IMAGES.with(|images| images.execute_module(binding)).map_err(AccessError::status)?
}
pub(crate) fn finish_module_images()
{
    let _ = IMAGES.with(|images| images.finish_module_retiring());
}
pub(crate) fn module_resource_selection(execution: &crate::images::ModuleExecution)
                                        -> ResourceSelection
{
    ResourceSelection { owner:  execution.module(),
                        image:  Some(execution.binding()),
                        system: None }
}
pub(crate) fn with_module_resources<R>(execution: &crate::images::ModuleExecution,
                                       call: impl FnOnce() -> R)
                                       -> Result<R, Status>
{
    crate::context::with_selected_resources(ResourceSelection { owner:  execution.module(),
                                                                image:  Some(execution.binding()),
                                                                system: None },
                                            call).map_err(|_| WRONG_THREAD)
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
    let Some(callback) = BOOT_LOG.get().copied().flatten()
    else
    {
        return OK;
    };
    // End application-byte borrows before switching roots. Only bounded
    // resident scratch, mapped identically in both roots, crosses the callback.
    let mut scratch = [0u8; 256];
    let mut offset = 0;
    while offset < length
    {
        let count = (length - offset).min(scratch.len());
        {
            let source = unsafe { core::slice::from_raw_parts(bytes.add(offset), count) };
            scratch[..count].copy_from_slice(source);
        }
        match crate::machine::paging::with_firmware(|| unsafe { callback(scratch.as_ptr(), count) })
        {
            Ok(OK) => (),
            Ok(status) => return status,
            Err(error) =>
            {
                crate::machine::paging::report_refusal(&error);
                return BAD_ABI;
            }
        }
        offset += count;
    }
    OK
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
    crate::machine::io::run(|| FILES.with(call))?
}
/// A refused firmware handoff returns its unconsumed cleanup capability.
pub(crate) fn with_files_owned<A, R>(argument: A,
                                     call: impl FnOnce(A, &mut crate::toolbox::files::Manager) -> R)
                                     -> Result<R, (AccessError, A)>
{
    crate::machine::io::run_owned(argument, |argument| FILES.with_owned(argument, call))?
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
pub(crate) fn with_scrap_owned<A, R>(argument: A,
                                     call: impl FnOnce(A, &mut crate::toolbox::scrap::Manager) -> R)
                                     -> Result<R, (AccessError, A)>
{
    SCRAP.with_owned(argument, call)
}
pub(crate) fn fonts() -> Option<&'static crate::toolbox::quickdraw::fonts::FontManager>
{
    FONTS.get()
}
/// Reclaimable rendering caches must leave room for application loading and
/// thread stacks. Read under the allocator's own synchronization boundary.
pub(crate) fn heap_statistics() -> (u64, u64, u64)
{
    let (used, capacity) = HEAP.statistics();
    (crate::machine::memory::VM_SYSTEM_HEAP_BASE, used as u64, capacity as u64)
}

pub(crate) fn heap_available() -> usize
{
    HEAP.available()
}
/// Authenticated source-selection metadata. Image invocations retain a Read
/// for their complete borrow; the explicit System proof instead names the
/// canonical reset-lifetime catalog and never depends on a recipient image.
#[derive(Clone, Copy)]
pub(crate) struct ResourceSelection
{
    owner:  u64,
    image:  Option<crate::images::Binding>,
    system: Option<SystemResources>
}
struct SystemResourceOwner
{
    owner: crate::toolbox::handles::OwnerRef
}

/// Copyable proof of the actual reset-lifetime System resource source. The
/// retained authority always comes from the canonical System-owner factory;
/// a logical recipient, missing app image or numeric zero cannot construct it.
#[derive(Clone, Copy)]
pub(crate) struct SystemResources(&'static SystemResourceOwner);

pub(crate) fn system_resources(owner: &crate::toolbox::handles::OwnerRef)
                               -> Result<SystemResources, Status>
{
    if owner.owner().kind() != mac_object_table::domain::DomainKind::System
    {
        return Err(ACCESS_DENIED);
    }
    static SOURCE: Once<SystemResourceOwner> = Once::new();
    let source =
        SOURCE.try_call_once(|| {
                  crate::toolbox::handles::system_owner().map(|owner| SystemResourceOwner { owner })
              })?;
    // Both values are genuine capabilities. Full identity comparison validates
    // the offered cap against the retained canonical factory result; it never
    // manufactures an authority from that identity. Unique System issuance and
    // private OwnerIndex construction prevent a second ledger for this owner.
    if source.owner.owner() != owner.owner()
    {
        return Err(ACCESS_DENIED);
    }
    Ok(SystemResources(source))
}
impl SystemResources
{
    pub(crate) fn same_source(&self, other: Self) -> bool
    {
        core::ptr::eq(self.0, other.0)
    }

    pub(crate) fn with<R>(&self, operation: impl FnOnce() -> R) -> Result<R, Status>
    {
        let owner = self.0.owner.owner().incarnation();
        let selection = ResourceSelection { owner,
                                            image: None,
                                            system: Some(*self) };
        crate::context::with_selected_resources(selection, operation).map_err(|_| WRONG_THREAD)
    }
}

pub(crate) fn with_system_resources<R>(owner: &crate::toolbox::handles::OwnerRef,
                                       operation: impl FnOnce() -> R)
                                       -> Result<R, Status>
{
    system_resources(owner)?.with(operation)
}

/// Definition lookup follows the installed authentic source role. System UI
/// cannot be shadowed by an application's resource search chain.
pub(crate) fn definition_resource_scope() -> mac_abi::resources::Scope
{
    if current_resource_selection().system.is_some()
    {
        mac_abi::resources::SYSTEM
    }
    else
    {
        mac_abi::resources::CHAIN
    }
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
                        image: if owner == 0 { None } else { context.image() },
                        system: None }
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
    crate::context::with_selected_resources(ResourceSelection { owner:  app,
                                                                image:  index,
                                                                system: None },
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

fn launch(info: &BootInfo) -> Result<(), crate::machine::scheduler::StartError>
{
    let ram_end = info.ram_base
                      .checked_add(info.ram_size)
                      .ok_or(mac_formats::Error::Malformed("RAM range overflow"))?;
    let module_arena = crate::machine::memory::module_arena(ram_end).map_err(|_| {
                           mac_formats::Error::Malformed("module RAM reservation unavailable")
                       })?;
    crate::drivers::initialize(module_arena,crate::machine::MODULE_TARGET).map_err(|_|mac_formats::Error::Malformed("module runtime initialization failed"))?;
    crate::toolbox::scrap::extension_runtime::initialize().map_err(|_| mac_formats::Error::Malformed("Scrap extension service initialization failed"))?;
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
    let finder_allocation =
        processes.allocation(finder_id)
                 .map_err(|_| {
                     mac_formats::Error::Malformed("Finder allocation authority unavailable")
                 })?;
    let finder_loaded = crate::allocator::with_target(crate::allocator::Target::Owner {
        account: finder_allocation, class: mac_kernel_model::policy::MemoryClass::Kernel
    }, || admit_image(&finder_owner,
                       AppImage { resources: finder,
                                  role: crate::images::Role::Application { authority },
                                  creator: *b"MACS",
                                  file_type: *b"FNDR" })).map_err(|cause| {
                            serial!("FINDER_IMAGE_REFUSED: {cause}");
                            match cause
                            {
                                crate::images::LoadError::Admission(_) =>
                                    crate::loader::LoadError::from(mac_formats::Error::Malformed(
                                        "Finder image admission failed")),
                                crate::images::LoadError::Write(cause) => cause
                            }
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
        ResourceSelection { owner:  binding.owner(),
                            image:  binding.image(),
                            system: None }
    };
    crate::context::set_finder_app(finder_id);
    crate::context::with_bootstrap_resources(Some(selection), || {
        initialize_desktop(system, processes, finder_loaded)
    })?;
    let released = current_resource_selection();
    if released.owner != 0 || released.image.is_some()
    {
        return Err(mac_formats::Error::Malformed("bootstrap resource scope survived desktop \
                                                  initialization").into());
    }
    serial!("BOOT_RESOURCE_SCOPE_RELEASED");
    // End the bootstrap resource scope and drop all admission guards before
    // leaving this stack permanently. Scheduled threads own their actual pins.
    crate::machine::scheduler::start()
}

fn recovery_boot_error(service: &'static str, status: Status) -> mac_formats::Error
{
    serial!("RECOVERY_BOOT_REFUSED service={service} status={status}");
    match status
    {
        NO_MEMORY => mac_formats::Error::AllocationFailed(service),
        NOT_FOUND => mac_formats::Error::Missing(service),
        TRANSLATION_REQUIRED => mac_formats::Error::TranslationRequired(service),
        _ => mac_formats::Error::Malformed(service)
    }
}

fn initialize_desktop(mut system: alloc::vec::Vec<mac_formats::resource::Resource>,
                      processes: crate::toolbox::processes::Manager, finder_loaded: LoadedImage)
                      -> crate::loader::LoadResult<()>
{
    let finder_entry = finder_loaded.entry();
    let finder_owner = processes.ledger(crate::context::finder_app().0).map_err(|_| {
        mac_formats::Error::Malformed("Finder bootstrap owner is unavailable")
    })?;
    let fonts =
        crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
            crate::toolbox::quickdraw::fonts::FontManager::take_resources(&mut system)
        }).map_err(|status| {
              match status
              {
                  mac_abi::NO_MEMORY =>
                  {
                      mac_formats::Error::AllocationFailed("cannot allocate the System font \
                                                            catalog")
                  }
                  _ => mac_formats::Error::Malformed("cannot initialize the System font catalog")
              }
          })?;
    serial!("FONTS_READY: families={} resources loaded from System", fonts.catalog().len());
    // Progress uses the real loaded CDEF before any application loop exists.
    let fonts = FONTS.call_once(|| fonts);
    let files = RESOURCE_FILES.call_once(|| crate::toolbox::resources::manager::Files { system });
    crate::toolbox::standard_file::prepare_templates().map_err(|status| {
                                                          recovery_boot_error("Standard File \
                                                                               template admission \
                                                                               failed",
                                                                              status)
                                                      })?;
    crate::toolbox::dialogs::core_alerts::prepare().map_err(|status| {
                                                       recovery_boot_error("System core alert \
                                                                            admission failed",
                                                                           status)
                                                   })?;
    crate::toolbox::crashes::initialize_protected_report(crate::recovery::permit()
        .map_err(|status| recovery_boot_error("protected report storage unavailable", status))?)
        .map_err(|status| recovery_boot_error("full report storage admission failed", status))?;
    crate::recovery::prepare_modal().map_err(|status| {
                                        recovery_boot_error("System modal service admission failed",
                                                            status)
                                    })?;
    let mut definitions = crate::loader::Definitions::new(files);
    let mut controls = crate::toolbox::controls::objects::Manager::new(
        crate::toolbox::handles::controls_kinds().map_err(|status| recovery_boot_error("Control Manager kind admission failed", status))?)
        .map_err(|status| recovery_boot_error("Control Manager admission failed", status))?;
    let mut boot_screen = crate::boot_screen::BootScreen::begin(&files.system, fonts,
                                                                &mut definitions, &mut controls)?;
    if !crate::platform::setup_framebuffer()
    {
        return Err(mac_formats::Error::Malformed("RAM framebuffer is unavailable").into());
    }
    serial!("BOOT_PANEL_READY stages=7");
    // Immutable publication is acquire/release synchronized by Once. Runtime
    // readers never spin or take a mutable catalog guard.
    boot_screen.completed(1, &mut controls)?;
    boot_screen.completed(2, &mut controls)?;
    crate::toolbox::files::types::initialize().map_err(|status| {
                                                  serial!("FILE_TYPES_ERROR status={}", status);
                                                  mac_formats::Error::Malformed("File Types is \
                                                                                 invalid or \
                                                                                 unreadable")
                                              })?;
    // Admit the one real resident UI and its ordinary-backed indices/pixel
    // stock before any installed module initializer can consume that heap.
    // BootScreen retains its Control identity without borrowing the manager;
    // move that manager once and keep the welcome panel until publication.
    let mut runtime = crate::toolbox::runtime::Desktop::new(definitions, processes, finder_owner,
                                                            controls).map_err(|cause| {
                          match cause
                          {
                              crate::toolbox::runtime::AdmissionError::Service(status) =>
                                  recovery_boot_error("resident Desktop admission failed", status).into(),
                              crate::toolbox::runtime::AdmissionError::Definition(cause) => cause
                          }
                      })?;
    runtime.prepare_recovery_storage().map_err(|cause| {
                                           match cause
                                           {
                                               crate::toolbox::runtime::AdmissionError::Service(status) =>
                                                   recovery_boot_error("System UI publication storage admission failed", status).into(),
                                               crate::toolbox::runtime::AdmissionError::Definition(cause) => cause
                                           }
                                       })?;
    runtime.prepare_ordinary_baseline().map_err(|status| {
                                            recovery_boot_error("initial ordinary desktop \
                                                                 admission failed",
                                                                status)
                                        })?;
    if let Some(storage) = crate::allocator::recovery::statistics()
    {
        serial!("RECOVERY_STORAGE_READY capacity={} admitted={} foundation={} system={} \
                 domains={} raw_backing={} full_recovery_kit=false",
                storage.capacity,
                storage.admitted,
                storage.foundation,
                storage.system,
                storage.domains,
                storage.raw_backing);
    }
    // Module-owned icon families resolve the System palettes at admission.
    // Publish immutable resource/font services before entering any driver;
    // exports still mount before Finder or another application can run.
    crate::drivers::discover(&mut boot_screen).map_err(|status| {
                                                  serial!("MODULE_DISCOVERY_REFUSED \
                                                           status={status}; inspect package \
                                                           admission and installation directory \
                                                           limits");
                                                  match status
        {
            NO_MEMORY => mac_formats::Error::AllocationFailed("module discovery storage refused"),
            NOT_FOUND => mac_formats::Error::Missing("module installation directory unavailable"),
            TRANSLATION_REQUIRED =>
            {
                mac_formats::Error::TranslationRequired("module code requires translation")
            }
            _ => mac_formats::Error::Malformed("module discovery failed; see recorded status")
        }
                                              })?;
    if let Err(status) = crate::toolbox::files::mount_startup_exports()
    {
        serial!("HOST_VOLUME_DISCOVERY_FAILED status={}", status);
    }
    runtime.boot_progress(&mut boot_screen, 3)?;
    // Keep hardware activation at its original post-discovery boundary. UI
    // admission above must not begin queueing boot-time input earlier.
    let devices = crate::virtio_input::initialize();
    if devices != 2
    {
        serial!("INPUT_UNAVAILABLE devices={}", devices);
        return Err(recovery_boot_error("native input initialization refused", NOT_SUPPORTED).into());
    }
    serial!("VIRTIO_INPUT=READY devices={}", devices);
    serial!("UI_OBSERVER address={} bytes=128 version=1",
            crate::toolbox::quickdraw::observer::address());
    runtime.boot_progress(&mut boot_screen, 4)?;
    DESKTOP.publish(runtime)
           .map_err(|_| mac_formats::Error::Malformed("desktop already published"))?;
    with_desktop(|desktop| desktop.boot_progress(&mut boot_screen, 5))
        .map_err(|error| recovery_boot_error("boot progress manager unavailable", error.status()))??;
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
    with_desktop(|desktop| desktop.boot_progress(&mut boot_screen, 6))
        .map_err(|error| recovery_boot_error("boot progress manager unavailable", error.status()))??;
    with_desktop(|desktop| desktop.started(crate::context::finder_app().0, thread)).map_err(|_| {
                                                          mac_formats::Error::Malformed("Finder context \
                                                                              initialization \
                                                                              failed")
                                                      })?;
    with_desktop(|desktop| desktop.boot_progress(&mut boot_screen, 7))
        .map_err(|error| recovery_boot_error("boot progress manager unavailable", error.status()))??;
    with_desktop(|desktop| desktop.finish_boot_progress(&mut boot_screen))
        .map_err(|error| recovery_boot_error("boot progress cleanup unavailable", error.status()))??;
    drop(boot_screen);
    let allocations = crate::allocator::statistics();
    serial!("RESIDENT_ALLOCATION_READY capacity={} admitted={} foundation={} system={} \
             domains={} header_bytes={} private_app_and_module_heaps=false",
            allocations.capacity,
            allocations.admitted,
            allocations.foundation,
            allocations.ordinary,
            allocations.domains,
            crate::allocator::HEADER_BYTES);
    Ok(())
}

/// # Safety
/// Bootstrap supplies a valid header and compatible BootInfo for this call.
/// Startup callbacks/context remain valid while System or Finder is running.
pub unsafe extern "C" fn start(payload: *const (), bytes: usize, role: u64)
                               -> *const SystemServices
{
    // Dispatch before STARTED, BOOT/header/context access or any global service
    // initialization. Warm entry never re-enters the cold primary path.
    match role
    {
        mac_abi::boot::monitor::ENTRY_SECONDARY =>
        {
            // SAFETY: the entry contract supplies the actual monitor payload;
            // private warm admission authenticates its row before reading it.
            unsafe { crate::machine::traps::start_secondary(payload, bytes) }
        }
        mac_abi::boot::monitor::ENTRY_PRIMARY if bytes == size_of::<BootInfo>() => (),
        _ =>
        {
            crate::machine::emergency::serial(format_args!("SYSTEM_ENTRY_REJECTED \
                                                            role={role:#x} bytes={bytes}; \
                                                            rebuild ROM and System together\n"));
            return ptr::null();
        }
    }
    let info = payload.cast::<BootInfo>();
    if info.is_null()
    {
        return ptr::null();
    }
    let header = unsafe { &*info.cast::<Header>() };
    if !header.supports::<BootInfo>() || header.size as usize != size_of::<BootInfo>()
    {
        crate::machine::paging::report_refusal(&format_args!("native BootInfo layout is \
                                                              incompatible: version {} size {}, \
                                                              expected version {} size {}; \
                                                              rebuild ROM and System together",
                                                             header.version,
                                                             header.size,
                                                             ABI_VERSION,
                                                             size_of::<BootInfo>()));
        return ptr::null();
    }
    let info: &'static BootInfo = unsafe { &*info };
    if info.flags & !mac_abi::BOOT_GRAPHICAL != 0
    {
        return ptr::null();
    }
    use core::sync::atomic::Ordering;
    // Secondary harts and repeated entry cannot replace live manager state or
    // race allocator initialization. Failed initialization is not retried over
    // partially published state; reset is the only supported bootstrap retry.
    if STARTED.load(Ordering::Acquire)
    {
        return ptr::null();
    }
    if let Err(error) = unsafe { crate::machine::paging::admit(info) }
    {
        crate::machine::paging::report_refusal(&error);
        return ptr::null();
    }
    if STARTED.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err()
    {
        return ptr::null();
    }
    let Some(boot_hart) = crate::machine::paging::firmware_owner_hart()
    else
    {
        return ptr::null();
    };
    if let Err(error) = crate::machine::traps::admit_bootstrap(boot_hart)
    {
        crate::machine::paging::report_refusal(&error);
        return ptr::null();
    }
    if crate::context::bootstrap_thread() != crate::context::BOOT_THREAD
    {
        return ptr::null();
    }
    BOOT_LOG.call_once(|| info.log);
    let (memory, mapped) = match crate::machine::paging::memory()
    {
        Ok(memory) => memory,
        Err(error) =>
        {
            serial!("BOOT_MEMORY_REJECTED: {error}");
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
    if let Err(error) = unsafe { crate::machine::pages::initialize(memory, mapped) }
    {
        serial!("BOOT_MEMORY_REJECTED: {error}");
        return ptr::null();
    }
    match unsafe {
              crate::machine::paging::initialize(|stack| {
                  let entry = match crate::machine::traps::prepare_entry(stack)
                  {
                      Ok(entry) => entry,
                      Err(error) =>
                      {
                          serial!("BOOT_SYSTEM_STACK_REJECTED: {error}");
                          return ptr::null();
                      }
                  };
                  entry.run(|| start_services(info))
              })
          }
    {
        Ok(services) => services,
        Err(error) =>
        {
            serial!("BOOT_TRANSLATION_REJECTED: {error}");
            ptr::null()
        }
    }
}
/// # Safety
/// Entry validated the pinned BootInfo and switched onto the owned System stack.
unsafe fn start_services(info: &BootInfo) -> *const SystemServices
{
    if info.flags & mac_abi::BOOT_GRAPHICAL != 0
       && let Err(error) = crate::machine::emergency::prepare()
    {
        crate::machine::emergency::serial(format_args!("BOOT_EMERGENCY_KIT_REJECTED: {error}"));
        return ptr::null();
    }
    let Some((heap_base, heap_bytes)) = crate::machine::paging::heap_range()
    else
    {
        crate::machine::emergency::serial(format_args!("BOOT_HEAP_REJECTED: permanent heap \
                                                        mapping/backing was not installed"));
        return ptr::null();
    };
    // SAFETY: permanent roots retain unique physical backing for this range.
    // This is the first heap initialization and no ordinary allocation has run.
    unsafe {
        HEAP.initialize(heap_base as *mut u8, heap_bytes);
    }
    if info.flags & mac_abi::BOOT_GRAPHICAL != 0
    {
        if let Err(status) = crate::recovery::initialize()
        {
            serial!("BOOT_RECOVERY_STORAGE_REFUSED status={status}");
            return ptr::null();
        }
        let prepared = crate::recovery::permit()
            .and_then(crate::machine::scheduler::initialize_finder_recovery);
        if let Err(status) = prepared
        {
            serial!("BOOT_FINDER_RECOVERY_BACKING_REFUSED status={status}");
            return ptr::null();
        }
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
        serial!("SYSTEM_STARTUP_FAILED: {}", &error);
        // Boot admission cannot retry over published reset-lifetime state.
        // Keep any owned idle-install refusal on this abandoned stack and in
        // its actual canonical Store/core until reset. No cleanup ACK or panic
        // is fabricated merely to return an unowned numeric startup failure.
        crate::machine::power::fatal_shutdown();
    }
    &SERVICES
}

pub fn allocation_failed(layout: Layout) -> !
{
    crate::machine::emergency::enter(crate::emergency::Kind::OutOfMemory,
                                     format_args!("Resident System allocation failed: {} bytes, \
                                                   alignment {}. Allocator statistics are not \
                                                   read on the failure path.",
                                                  layout.size(),
                                                  layout.align()))
}

/// Native-only qualification rides an authenticated faulting definition in a
/// private image; it adds no application-visible trap or production switch.
#[cfg(feature = "emergency-qualification")]
pub(crate) fn qualify_emergency_with_allocator_locked() -> !
{
    #[cfg(feature = "emergency-qualification-oom")]
    {
        use core::alloc::GlobalAlloc;

        let (_, capacity) = HEAP.statistics();
        let layout = match capacity.checked_add(1)
                                   .and_then(|size| Layout::from_size_align(size, 4096).ok())
        {
            Some(layout) => layout,
            None =>
            {
                crate::machine::emergency::enter(crate::emergency::Kind::SystemPanic,
                                                 format_args!("Emergency qualification layout \
                                                               could not be represented"))
            }
        };
        // Invoke the real backend directly. Rust's global allocation intrinsic
        // may elide an unused allocation/deallocation pair and assume success;
        // that would qualify an optimizer assumption instead of heap refusal.
        let pointer = unsafe { HEAP.alloc(layout) };
        if !pointer.is_null()
        {
            unsafe {
                HEAP.dealloc(pointer, layout);
            }
            crate::machine::emergency::enter(crate::emergency::Kind::SystemPanic,
                                             format_args!("Emergency qualification expected \
                                                           real allocator refusal"));
        }
        crate::machine::emergency::serial(format_args!("EMERGENCY_QUALIFICATION_OOM_REFUSED \
                                                        bytes={} align={}",
                                                       layout.size(),
                                                       layout.align()));
        HEAP.qualify_while_locked(|| alloc::alloc::handle_alloc_error(layout))
    }
    #[cfg(not(feature = "emergency-qualification-oom"))]
    HEAP.qualify_while_locked(|| {
            panic!("EMERGENCY_QUALIFICATION_PANIC: allocator and ordinary Desktop locks are held")
        })
}
pub fn panic(info: &PanicInfo<'_>) -> !
{
    // PanicInfo's Display may invoke an arbitrary application-supplied
    // formatter, including one waiting on the very lock we interrupted. Only
    // borrow already-captured text and primitive source fields on this path.
    let message =
        info.message()
            .as_str()
            .unwrap_or("Dynamic panic arguments were not evaluated: their formatter may require \
                        a failed allocator or an interrupted lock.");
    match info.location()
    {
        Some(location) =>
        {
            crate::machine::emergency::enter(crate::emergency::Kind::SystemPanic,
                                             format_args!("{message}\nSource: {}:{}:{}",
                                                          location.file(),
                                                          location.line(),
                                                          location.column()))
        }
        None =>
        {
            crate::machine::emergency::enter(crate::emergency::Kind::SystemPanic,
                                             format_args!("{message}\nSource unavailable"))
        }
    }
}
