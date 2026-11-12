//! Resident kernel entry, service boundary and startup orchestration.
use core::{alloc::Layout,
           fmt::{self, Write},
           panic::PanicInfo,
           ptr};

use mac_abi::*;
use spin::Once;

mod desktop;
mod window_work;
mod control_work;
pub(crate) use desktop::{Epoch as DesktopEpoch, arm_control_acknowledgement,
                         control_resource_acknowledged, epoch as window_desktop_epoch,
                         source_returned as window_sources_changed};

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
static RESOURCE_FILES: Once<crate::toolbox::resources::manager::Files> = Once::new();
static FONTS: Once<alloc::sync::Arc<crate::toolbox::quickdraw::fonts::FontManager>> = Once::new();
static INSTALLED_FONTS:
    spinning_top::Spinlock<Option<alloc::sync::Arc<crate::toolbox::quickdraw::fonts::FontManager>>> =
    spinning_top::Spinlock::new(None);
static TOOLBOX: desktop::Desktop = desktop::Desktop::new();
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

pub(crate) fn load_application(owner: &crate::toolbox::handles::OwnerRef,
                               account: &crate::machine::scheduler::accounting::AllocationAuthority,
                               creator: [u8; 4], file_type: [u8; 4],
                               resources: alloc::vec::Vec<mac_formats::resource::Resource>)
                               -> Result<LoadedImage, Status>
{
    let authority = application_authority(&resources)?;
    admit_image(owner,
                account,
                AppImage { role: crate::images::Role::Application { authority },
                           resources,
                           creator,
                           file_type }).map_err(application_image_status)
}
fn admit_image(owner: &crate::toolbox::handles::OwnerRef,
               account: &crate::machine::scheduler::accounting::AllocationAuthority,
               source: AppImage)
               -> Result<LoadedImage, ApplicationImageError>
{
    // Immutable package sources are interned by the System image registry.
    // Their resource backing is attributed at the storage read, before this move.
    let source = crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
                     alloc::sync::Arc::try_new(source)
                 }).map_err(|_| NO_MEMORY)?;
    prepare_shared_image(owner, account, source)?.write_application(account)
}

/// Publish the replacement's genuine source alias without allocating its image
/// payload yet. The pending owner retains protected authority explicitly; the
/// caller decides when the old Process has actually retired before advancing.
pub(crate) fn prepare_recovery_application(owner: &crate::toolbox::handles::OwnerRef,
                                           account: &crate::machine::scheduler::accounting::AllocationAuthority,
                                           original: crate::images::Binding,
                                           permit: crate::allocator::recovery::Permit)
                                           -> Result<crate::images::PendingRecoveryImage, Status>
{
    crate::context::with_recovery_allocation(permit.clone(), || {
        crate::allocator::with_target(crate::allocator::Target::Owner {
            account: account.clone(), class: mac_kernel_model::policy::MemoryClass::Kernel
        }, || {
            // Keep this genuine canonical Read through the new alias birth.
            // A copied Source Arc alone cannot authorize prepared sharing.
            let original = IMAGES.with(|images| images.read(original))
                                 .map_err(AccessError::status)??;
            let layout = crate::loader::application_layout(&original.source().resources)
                .map_err(crate::loader::status)?;
            let plan = IMAGES.with(|images| images.plan_recovery_share(&original, owner, account, &layout))
                             .map_err(AccessError::status)??;
            let prepared = plan.prepare()?;
            let (sharing, retired) = match IMAGES.with_owned(prepared, |prepared, images| {
                images.admit_recovery_share(prepared)
            })
            {
                Ok(Ok(admitted)) => admitted,
                Ok(Err(failure)) =>
                {
                    let status = failure.cause;
                    drop(failure.preparation);
                    return Err(status);
                }
                Err((cause, prepared)) =>
                {
                    drop(prepared);
                    return Err(cause.status());
                }
            };
            retired.dispose();
            let prepared = match sharing.prepare(&original, layout)
            {
                Ok(prepared) => prepared,
                Err((cause, sharing)) =>
                {
                    drop(sharing);
                    return Err(cause);
                }
            };
            let (prepared, retired) = match IMAGES.with_owned(prepared, |prepared, images| {
                images.publish_recovery_copy(&original, prepared)
            })
            {
                Ok(Ok(published)) => published,
                Ok(Err(failure)) =>
                {
                    let status = failure.cause;
                    drop(failure.preparation);
                    return Err(status);
                }
                Err((cause, prepared)) =>
                {
                    drop(prepared);
                    return Err(cause.status());
                }
            };
            retired.dispose();
            drop(original); // new canonical alias now retains the same target
            prepared.into_recovery(permit).map_err(|(cause, prepared)| {
                drop(prepared);
                cause.status()
            })
        })
    })
}

/// Clone immutable package ownership under IMAGES, then admit dependencies
/// without retaining its guard across registry or future provider calls.
pub(crate) fn select_application_services(owner: &crate::toolbox::handles::OwnerRef,
                                          allocation: crate::machine::scheduler::accounting::AllocationAuthority,
                                          tls: &crate::toolbox::native_threads::RootBinding,
                                          image: crate::images::Binding)
                                          -> Result<Option<u64>, Status>
{
    let source = IMAGES.with(|images| images.read(image).map(|read| read.shared_source()))
                       .map_err(AccessError::status)??;
    crate::toolbox::processes::select_required_services(&source.resources, owner, allocation, tls)
        .map_err(|cause| cause.status())
}

fn prepare_shared_image(owner: &crate::toolbox::handles::OwnerRef,
                        account: &crate::machine::scheduler::accounting::AllocationAuthority,
                        source: alloc::sync::Arc<AppImage>)
                        -> Result<crate::images::PreparedLoad, ApplicationImageError>
{
    if !matches!(source.role, crate::images::Role::Application { .. })
    {
        return Err(crate::images::LoadError::Admission(ACCESS_DENIED));
    }
    let layout = crate::loader::application_layout(&source.resources)
        .map_err(|cause| crate::images::LoadError::Write(cause.into()))?;
    let prepared = prepare_image_source(owner,
                                        account,
                                        source,
                                        layout.preferred,
                                        layout.preferred,
                                        layout.extent,
                                        crate::machine::memory::VM_APPLICATION_IMAGE_BASE,
                                        crate::machine::memory::VM_APPLICATION_END,
                                        layout.executable,
                                        layout.writable)?;
    // The original alias already reserves its destination and retains its
    // source/loading ownership. Long byte writes and monitor waits borrow no
    // IMAGES guard. Callers must also leave Desktop before this boundary.
    Ok(prepared)
}
/// Real ordinary image factories and all refused owners leave IMAGES before Drop.
fn prepare_image_source(owner: &crate::toolbox::handles::OwnerRef,
                        account: &crate::machine::scheduler::accounting::AllocationAuthority,
                        source: alloc::sync::Arc<AppImage>, linked: u64, preferred: u64,
                        extent: u64, relocation: u64, limit: u64,
                        executable: alloc::vec::Vec<core::ops::Range<u64>>,
                        writable: alloc::vec::Vec<core::ops::Range<u64>>)
                        -> Result<crate::images::PreparedLoad, Status>
{
    crate::context::without_recovery(|| {
        let target =
            crate::allocator::Target::Owner { account: account.clone(),
                                              class:
                                                  mac_kernel_model::policy::MemoryClass::Kernel };
        crate::allocator::with_target(target, || {
            let plan = match IMAGES.with_owned(source, |source, images| {
                                       images.plan_load(owner, account, source, linked, preferred,
                                                        extent, relocation, limit)
                                   })
            {
                Ok(Ok(plan)) => plan,
                Ok(Err(failure)) =>
                {
                    serial!("IMAGE_INTERN_PLAN_REFUSED {}", failure.cause);
                    let status = failure.cause.status();
                    drop(failure.preparation);
                    return Err(status);
                }
                Err((cause, source)) =>
                {
                    drop(source);
                    return Err(cause.status());
                }
            };
            let preparation = plan.prepare(executable, writable)?;
            let (permit, retired) = match IMAGES.with_owned(preparation, |preparation, images| {
                                                    images.admit_load(preparation)
                                                })
            {
                Ok(Ok(result)) => result,
                Ok(Err(failure)) =>
                {
                    drop(failure.preparation);
                    return Err(failure.cause);
                }
                Err((cause, preparation)) =>
                {
                    drop(preparation);
                    return Err(cause.status());
                }
            };
            retired.dispose();
            let (prepared, retired) =
                match IMAGES.with_owned(permit, |permit, images| images.publish_load(permit))
                {
                    Ok(Ok(result)) => result,
                    Ok(Err(failure)) =>
                    {
                        failure.preparation.dispose();
                        return Err(failure.cause);
                    }
                    Err((cause, permit)) =>
                    {
                        drop(permit);
                        return Err(cause.status());
                    }
                };
            retired.dispose();
            Ok(prepared)
        })
    })
}
/// All resident DRVR/INIT code uses real page-backed supervisor mappings.
pub(crate) fn load_module(owner: &crate::toolbox::handles::OwnerRef,
                          account: &crate::machine::scheduler::accounting::AllocationAuthority,
                          source: alloc::sync::Arc<AppImage>, code: &mac_formats::code::NativeCode)
                          -> Result<LoadedImage, Status>
{
    if source.role != crate::images::Role::Module
       || owner.owner() != account.owner()
       || owner.owner().kind() != mac_object_table::domain::DomainKind::Module
    {
        return Err(ACCESS_DENIED);
    }
    let decoded =
        mac_formats::native::image::decode(&code.code, &code.descriptor).map_err(|_| BAD_ABI)?;
    let extent = code.descriptor.load_limit - code.descriptor.load_base;
    if !extent.is_multiple_of(crate::machine::memory::PAGE_BYTES)
       || !code.descriptor.load_base.is_multiple_of(crate::machine::memory::PAGE_BYTES)
    {
        serial!("MODULE_MAPPING_REFUSED: native module link reservation is not page aligned; \
                 rebuild the package");
        return Err(BAD_ABI);
    }
    let mut executable = alloc::vec::Vec::new();
    executable.try_reserve_exact(decoded.segments.len()).map_err(|_| NO_MEMORY)?;
    executable.extend(decoded.segments
                             .iter()
                             .filter(|s| {
                                 s.flags & mac_formats::native::image::EXECUTE != 0 && s.length != 0
                             })
                             .map(|s| {
                                 code.descriptor.load_base + s.offset
                                 ..code.descriptor.load_base + s.offset + s.length as u64
                             }));
    let prepared = prepare_image_source(owner,
                                        account,
                                        source,
                                        code.descriptor.load_base,
                                        crate::machine::memory::VM_MODULE_START,
                                        extent,
                                        crate::machine::memory::VM_MODULE_START,
                                        crate::machine::memory::VM_MODULE_END,
                                        executable,
                                        alloc::vec::Vec::new())?;
    prepared.write_module(account, code).map_err(|cause| {
                                            serial!("MODULE_MAPPING_REFUSED: {cause}");
                                            cause.status()
                                        })
}

pub(crate) fn module_dma_address(cpu: u64, bytes: u64) -> Result<u64, Status>
{
    use mac_object_table::shared::InvocationGate as _;
    crate::toolbox::handles::InvocationGate::run(|| {
        IMAGES.with(|images| images.module_dma_address(cpu, bytes))
              .map_err(AccessError::status)?
              .map_err(|cause| {
                  serial!("MODULE_DMA_REFUSED: {cause}");
                  cause.status()
              })
    })
}

/// Canonical guard-free maintenance. Original alias tickets remain in their
/// owner walk through PTE removal, genuine target ACK and physical refund.
pub(crate) fn pump_module_images()
{
    crate::machine::paging::user::pump();
    window_work::pump();
    control_work::pump();
    // Consume independent real notices without short-circuiting either one.
    let mappings = crate::machine::paging::modules::take_pending();
    let spaces = crate::machine::paging::process::take_retirement_pending();
    if spaces
    {
        if let Some(claim) = crate::machine::paging::process::retirement()
        {
            let owner = claim.owner();
            if let Err(cause) = claim.run()
            {
                if !matches!(cause, crate::machine::paging::process::Error::Threads
                    | crate::machine::paging::process::Error::Readers(_)
                    | crate::machine::paging::process::Error::Busy
                    | crate::machine::paging::process::Error::Monitor(crate::machine::monitor::Error::Busy))
                {
                    serial!("PROCESS_SPACE_RETIREMENT_RETAINED owner={owner} cause={cause}");
                }
            }
        }
    }
    if !mappings
    {
        return;
    }
    // DDEF and IMAG consume the same real mapping-change notice, independently.
    // An empty/blocked definition scan must not stop image maintenance (or vice
    // versa); neither scan holds Desktop while executing its physical claim.
    let mut definition_after = 0;
    loop
    {
        let claim =
            match crate::toolbox::resources::definitions::mapping_retirement(definition_after)
            {
                Ok(Some(claim)) => claim,
                Ok(None) => break,
                Err(cause) =>
                {
                    serial!("DEFINITION_MAPPING_RETIREMENT_RETAINED: {cause}; original ticket \
                             retained");
                    break;
                }
            };
        definition_after = claim.base();
        if let Err(cause) = claim.run()
        {
            if !matches!(cause, crate::machine::paging::modules::Error::Monitor(
                crate::machine::monitor::Error::Busy))
            {
                serial!("DEFINITION_MAPPING_RETIREMENT_RETAINED base={definition_after:#x} \
                         cause={cause}");
            }
        }
    }
    let mut after = 0;
    loop
    {
        let next = crate::machine::interrupts::with_masked(|| {
            IMAGES.with(|images| images.mapping_retirement(after))
        });
        let claim = match next
        {
            Ok(Some(claim)) => claim,
            Ok(None) => break,
            Err(AccessError::Busy) =>
            {
                // No claim consumed the real retirement notice. IMAGES is a
                // bounded metadata turn; retry its actual contention instead
                // of waiting for an unrelated reader/monitor event to wake us.
                crate::machine::paging::modules::changed();
                break;
            }
            Err(cause) =>
            {
                serial!("IMAGE_MAPPING_RETIREMENT_RETAINED: image manager admission refused \
                         ({cause:?}); original mapping and cleanup ticket remain retained");
                break;
            }
        };
        after = claim.owner();
        if let Err(cause) = claim.run()
        {
            // Busy is rearmed by the actual monitor lease's release, not by
            // every input/frame. Other refusals retain their canonical cause.
            if !matches!(cause,crate::machine::paging::modules::Error::Monitor(crate::machine::monitor::Error::Busy)) {
                serial!("IMAGE_MAPPING_RETIREMENT_RETAINED owner={after} cause={cause}");
            }
        }
    }
    finish_module_images();
}
pub(crate) fn module_execution(binding: crate::images::Binding)
                               -> Result<crate::images::ModuleExecution, Status>
{
    use mac_object_table::shared::InvocationGate as _;
    // Host continuations are preemptible even when pinned to the primary.
    // Mask before acquiring IMAGES, through disposal of its outer guard: an
    // inner alias/account gate alone leaves that guard exposed to suspension.
    // Only the bounded lookup and execution-pin acquisition run here; module
    // entry, code writes and foreign dispatch stay outside this metadata turn.
    crate::toolbox::handles::InvocationGate::run(|| {
        IMAGES.with(|images| images.execute_module(binding)).map_err(AccessError::status)?
    })
}
pub(crate) fn application_execution(owner: &crate::toolbox::handles::OwnerRef,
                                    binding: crate::images::Binding)
                                    -> Result<crate::images::ApplicationExecution, Status>
{
    use mac_object_table::shared::InvocationGate as _;
    crate::toolbox::handles::InvocationGate::run(|| {
        IMAGES.with(|images| images.execute_application(owner, binding))
              .map_err(AccessError::status)?
    })
}
/// Caller supplies the genuine Process executor's owner/image, never raw app
/// IDs. The returned guard owns initialized backing beyond this metadata turn.
pub(crate) fn application_atomic32_pin(owner: &crate::toolbox::handles::OwnerRef,
                                       binding: crate::images::Binding, address: usize)
                                       -> Result<crate::images::ApplicationAtomic32, Status>
{
    use mac_object_table::shared::InvocationGate as _;
    crate::toolbox::handles::InvocationGate::run(|| {
        IMAGES.with(|images| images.pin_application_atomic32(owner, binding, address))
              .map_err(AccessError::status)?
    })
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
pub(crate) fn finish_image_origins()
{
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
/// Fieldwise bytes for a separate readonly user mapping. No resident address
/// or potentially uninitialized Rust padding is copied into the application.
pub(crate) fn user_service_descriptor() -> [u8; 16]
{
    let mut bytes = [0; 16];
    bytes[..4].copy_from_slice(&SERVICES.header.version.to_ne_bytes());
    bytes[4..8].copy_from_slice(&SERVICES.header.size.to_ne_bytes());
    bytes[8..].copy_from_slice(&SERVICES.capabilities.to_ne_bytes());
    bytes
}
static STARTED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

static SERVICES: SystemServices =
    SystemServices { header:       Header::for_type::<SystemServices>(),
                     capabilities: CAP_GRAPHICS | CAP_EVENT_WAIT | CAP_MONOTONIC_CLOCK };
static HEADLESS_SERVICES: SystemServices = SystemServices { header:
                                                                Header::for_type::<SystemServices>(),
                                                            capabilities: CAP_MONOTONIC_CLOCK };

pub(crate) unsafe extern "C" fn log(bytes: *const u8, length: usize) -> Status
{
    if bytes.is_null() && length != 0
    {
        return BAD_ARGUMENT;
    }
    if length == 0
    {
        return OK;
    }
    // System's own console, not the firmware callback. Routing log lines through
    // firmware meant every line switched translation roots, and the switch could
    // not itself report a fault: logging inside it recursed into this function,
    // and a fault during the switch was silent. The console needs no root switch,
    // so the whole hazard is gone rather than worked around.
    //
    // SAFETY: the caller supplies a readable buffer of `length` bytes.
    let text = unsafe { core::slice::from_raw_parts(bytes, length) };
    crate::machine::emergency::write_bytes(text);
    OK
}
// Mutable state never exposes unchecked static references. These guards do not
// wait for a preempted holder. Stateful recursive calls return Busy; immutable
// resources/fonts and independent Surface services stay available during WDEFs.
pub(crate) fn with_toolbox<R>(call: impl FnOnce(&mut crate::toolbox::runtime::Toolbox) -> R)
                              -> Result<R, AccessError>
{
    TOOLBOX.with(|toolbox| {
               toolbox.enter_caller();
               call(toolbox)
           })
}

/// Window continuations keep their original capability on Desktop refusal.
/// The successful holder's actual return, not Busy itself, posts availability.
pub(crate) fn with_window_toolbox_owned<A, R>(value: A,
                                              call: impl FnOnce(A,
                                                     &mut crate::toolbox::runtime::Toolbox)
                                                     -> R)
                                              -> Result<R, (AccessError, A)>
{
    TOOLBOX.with_window_owned(value, |value, toolbox| {
               toolbox.enter_caller();
               call(value, toolbox)
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
    let Ok((foreground, fresh_input)) = TOOLBOX.with(|toolbox| {
                                            if crate::toolbox::inspection::take_membership_change()
                                            {
                                                toolbox.notify_inspectors();
                                            }
                                            toolbox.wake_waiters(&mut waiting)
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
pub(crate) fn fonts() -> Option<alloc::sync::Arc<crate::toolbox::quickdraw::fonts::FontManager>>
{
    use mac_object_table::shared::InvocationGate;
    crate::toolbox::handles::InvocationGate::run(|| {
        INSTALLED_FONTS.lock().as_ref().or_else(|| FONTS.get()).cloned()
    })
}

pub(crate) fn publish_fonts(baseline: &alloc::sync::Arc<crate::toolbox::quickdraw::fonts::FontManager>,
                            fonts: alloc::sync::Arc<crate::toolbox::quickdraw::fonts::FontManager>)
                            -> Result<(), Status>
{
    use mac_object_table::shared::InvocationGate;
    let retired = crate::toolbox::handles::InvocationGate::run(|| {
        let mut slot = INSTALLED_FONTS.lock();
        let current = slot.as_ref().or_else(|| FONTS.get()).ok_or(NOT_SUPPORTED)?;
        if !alloc::sync::Arc::ptr_eq(current, baseline)
        {
            return Err(SCAN_STALE);
        }
        Ok(slot.replace(fonts))
    })?;
    drop(retired);
    Ok(())
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
    pub(crate) fn owner(&self) -> &crate::toolbox::handles::OwnerRef
    {
        &self.0.owner
    }

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
            Some(IMAGES.with(|images| images.read(binding)).map_err(|cause| {
                    // This is the primary-owned resource invocation boundary.
                    // Preserve the typed refusal and useful source context;
                    // ordinary successful resource reads produce no log work.
                    static REPORTED: core::sync::atomic::AtomicBool =
                        core::sync::atomic::AtomicBool::new(false);
                    if !REPORTED.swap(true, core::sync::atomic::Ordering::Relaxed)
                    {
                        serial!("RESOURCE_IMAGE_REFUSED owner={app} image={} cause={cause:?}; \
                                 further invocation refusals are not logged", binding.id());
                    }
                    cause.status()
                })??)
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
        FONTS.get().ok_or(NOT_SUPPORTED)?.resources()).with_images(&catalog, origin, primary.as_ref())))
}

fn launch() -> Result<(), crate::machine::scheduler::StartError>
{
    crate::drivers::initialize(crate::machine::MODULE_TARGET).map_err(|_|mac_formats::Error::Malformed("module runtime initialization failed"))?;
    crate::toolbox::scrap::extension_runtime::initialize().map_err(|_| mac_formats::Error::Malformed("Scrap extension service initialization failed"))?;
    crate::machine::driver_devices::initialize().map_err(|_| {
                                                    mac_formats::Error::Malformed("device service \
                                                                                   initialization \
                                                                                   failed")
                                                })?;
    crate::drivers::file_sources::runtime::initialize().map_err(|_| mac_formats::Error::Malformed("backing-file service initialization failed"))?;
    crate::toolbox::application_heap::initialize().map_err(|cause| {
        serial!("APPLICATION_HEAP_INITIALIZATION_FAILED: {cause}");
        recovery_boot_error("application heap manager initialization refused", cause.status())
    })?;
    crate::machine::paging::process::initialize().map_err(|cause| {
        serial!("PROCESS_SPACE_INITIALIZATION_REFUSED: {cause}");
        mac_formats::Error::Malformed("Process space manager admission failed")
    })?;
    crate::toolbox::native_threads::initialize().map_err(|cause| {
                                                    recovery_boot_error("native TLS manager \
                                                                         initialization refused",
                                                                        cause.status())
                                                })?;
    crate::machine::scheduler::initialize_records().map_err(|status| {
                                                       recovery_boot_error("canonical thread \
                                                                            Store initialization \
                                                                            refused",
                                                                           status)
                                                   })?;
    crate::machine::scheduler::application::initialize().map_err(|cause| {
        recovery_boot_error("native child manager initialization refused", cause.status())
    })?;
    let system = crate::loader::read_startup("System")?;
    let finder = crate::loader::read_startup_application("Finder")?;
    let original_catalog = finder.catalog;
    let mut processes = crate::toolbox::processes::Manager::new();
    let finder_id = processes.provision("Finder", original_catalog.volume, original_catalog.id)
                             .map_err(|status| {
                                 serial!("FINDER_DOMAIN_REFUSED status={status}");
                                 mac_formats::Error::AllocationFailed("Finder lifetime domain \
                                                                       admission failed")
                             })?;
    processes.bind_launch_catalog(finder_id, original_catalog).map_err(|status| {
        recovery_boot_error("Finder original catalog retention refused", status)
    })?;
    serial!("BOOT_FINDER_DOMAIN app={finder_id}");
    let finder_admission =
        processes.launch_admission(finder_id).map_err(|status| {
                                                  recovery_boot_error("Finder initial launch \
                                                                       admission refused",
                                                                      status)
                                              })?;
    let finder_owner =
        processes.ledger(finder_id)
                 .map_err(|_| mac_formats::Error::Malformed("Finder owner unavailable"))?;
    let kinds = crate::toolbox::handles::image_kinds().map_err(|_| mac_formats::Error::AllocationFailed("image kind admission failed"))?;
    IMAGES.publish(crate::images::Manager::new(crate::toolbox::handles::system_owner().map_err(|_| mac_formats::Error::AllocationFailed("System image owner unavailable"))?, kinds)
        .map_err(|_| mac_formats::Error::AllocationFailed("image manager allocation failed"))?)
        .map_err(|_| mac_formats::Error::Malformed("image manager already published"))?;
    let authority =
        policy_authority(&system, &finder.resources).map_err(|_| {
                                              mac_formats::Error::Malformed("invalid Finder \
                                                                             authority policy")
                                          })?;
    let finder_allocation =
        processes.allocation(finder_id)
                 .map_err(|_| {
                     mac_formats::Error::Malformed("Finder allocation authority unavailable")
                 })?;
    // The local boot directory already retains this genuine provisional owner
    // and checkpoint. Complete heap preparation occurs with no manager guard;
    // no application can enter before its own canonical root is attached.
    let heap_prepared = crate::toolbox::application_heap::prepare_root(
        finder_owner.clone(), finder_allocation.clone()).map_err(|cause| {
            serial!("FINDER_HEAP_PREPARATION_FAILED: {cause}");
            recovery_boot_error("Finder heap root preparation refused", cause.status())
        })?;
    let heap = match crate::toolbox::application_heap::publish_root(heap_prepared)
    {
        Ok(heap) => heap,
        Err(failure) =>
        {
            let crate::toolbox::application_heap::RootPublicationFailure { cause,
                                                                           prepared } = failure;
            serial!("FINDER_HEAP_PUBLICATION_FAILED: {cause}");
            prepared.dispose();
            return Err(recovery_boot_error("Finder heap root publication refused",
                                           cause.status()).into());
        }
    };
    processes.bind_heap(&finder_owner, heap).map_err(|status| {
                                                 recovery_boot_error("Finder heap root attachment \
                                                                      refused",
                                                                     status)
                                             })?;
    let tls_prepared =
        crate::toolbox::native_threads::prepare_root(finder_owner.clone(),
                                                     finder_allocation.clone()).map_err(|cause| {
            recovery_boot_error("Finder TLS root preparation refused", cause.status())
        })?;
    let tls = crate::toolbox::native_threads::publish_root(tls_prepared).map_err(|failure| {
                  let status = failure.cause.status();
                  failure.prepared.dispose();
                  recovery_boot_error("Finder TLS root publication refused", status)
              })?;
    processes.bind_tls(&finder_owner, tls).map_err(|status| {
                                               recovery_boot_error("Finder TLS root attachment \
                                                                    refused",
                                                                   status)
                                           })?;
    let finder_loaded = crate::allocator::with_target(crate::allocator::Target::Owner {
        account: finder_allocation.clone(), class: mac_kernel_model::policy::MemoryClass::Kernel
    }, || admit_image(&finder_owner, &finder_allocation,
                       AppImage { resources: finder.resources,
                                  role: crate::images::Role::Application { authority },
                                  creator: original_catalog.creator,
                                  file_type: original_catalog.file_type })).map_err(|cause| {
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
    // The scheduler never returns to this bootstrap stack. Only actual Process
    // roots and image payloads may retain this allocation authority afterward.
    drop(finder_allocation);
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
    let pending = crate::context::with_bootstrap_resources(Some(selection), || {
        initialize_desktop(system, processes, finder_loaded, finder_admission)
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
    crate::machine::scheduler::start(pending)
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

fn initialize_desktop(
    mut system: alloc::vec::Vec<mac_formats::resource::Resource>,
    processes: crate::toolbox::processes::Manager, finder_loaded: LoadedImage,
    finder_admission: crate::toolbox::processes::LaunchAdmission)
    -> crate::loader::LoadResult<Option<crate::toolbox::processes::startup::PendingStartup>>
{
    let finder_owner = processes.ledger(crate::context::finder_app().0).map_err(|_| {
        mac_formats::Error::Malformed("Finder bootstrap owner is unavailable")
    })?;
    let finder_tls =
        processes.tls(crate::context::finder_app().0).map_err(|status| {
                                                          recovery_boot_error("Finder runtime \
                                                                               dependency scope \
                                                                               unavailable",
                                                                              status)
                                                      })?;
    let finder_allocation = processes.allocation(crate::context::finder_app().0)
                                     .map_err(|status| {
                                         recovery_boot_error("Finder dependency account \
                                                              unavailable",
                                                             status)
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
    let fonts = alloc::sync::Arc::try_new(fonts).map_err(|_| {
                                                    recovery_boot_error("System font snapshot \
                                                                         admission failed",
                                                                        NO_MEMORY)
                                                })?;
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
    crate::loader::initialize_definitions()?;
    let system_source =
        system_resources(&crate::toolbox::handles::system_owner().map_err(|status| {
                              recovery_boot_error("System definition source unavailable", status)
                          })?).map_err(|status| {
                                  recovery_boot_error("System definition source unavailable",
                                                      status)
                              })?;
    let mut controls = crate::toolbox::controls::objects::Manager::new(
        crate::toolbox::handles::controls_kinds().map_err(|status| recovery_boot_error("Control Manager kind admission failed", status))?)
        .map_err(|status| recovery_boot_error("Control Manager admission failed", status))?;
    let mut boot_screen =
        crate::boot_screen::BootScreen::begin(&files.system, fonts, system_source, &mut controls)?;
    // ramfb is the bootstrap/emergency display. Without it System draws into
    // its reservation until an installed video driver attaches below.
    let ramfb = crate::platform::setup_framebuffer();
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
    let mut runtime = crate::toolbox::runtime::Toolbox::new(processes,
                                                            finder_owner.clone(),
                                                            controls).map_err(|cause| {
                                                                         match cause
            {
                crate::toolbox::runtime::AdmissionError::Service(status) =>
                {
                    recovery_boot_error("resident Desktop admission failed", status).into()
                }
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
    match crate::machine::video::attach()
    {
        Ok(true) => (),
        Ok(false) if ramfb => (),
        Ok(false) =>
        {
            return Err(mac_formats::Error::Malformed("no display: install a video driver or \
                                                      enable ramfb").into());
        }
        Err(status) if ramfb =>
        {
            serial!("VIDEO_DRIVER_UNAVAILABLE status={status}; staying on ramfb")
        }
        Err(status) =>
        {
            return Err(recovery_boot_error("video driver attach refused and ramfb is absent",
                                           status).into());
        }
    }
    let selected = select_application_services(&finder_owner,
                                               finder_allocation,
                                               &finder_tls,
                                               finder_loaded.binding()).map_err(|status| {
                       recovery_boot_error("Finder required provider admission refused", status)
                   })?;
    let ready =
        selected.map(|provider| {
                    crate::toolbox::processes::launch::ready_job(&finder_tls,
                                                                 finder_admission.owner(),
                                                                 provider)
                })
                .transpose()
                .map_err(|status| {
                    recovery_boot_error("Finder READY job admission refused", status)
                })?;
    if let Err(status) = crate::toolbox::files::mount_startup_exports()
    {
        serial!("HOST_VOLUME_DISCOVERY_FAILED status={}", status);
    }
    runtime.boot_progress(&mut boot_screen, 3)?;
    // Keep hardware activation at its original post-discovery boundary. UI
    // admission above must not begin queueing boot-time input earlier.
    let devices = crate::virtio_input::initialize();
    // A machine with a virtio bank is expected to carry the keyboard and the
    // pointer on it, and one that does not present both is refused. A machine
    // that declares no bank has no virtio input to be missing: it comes up
    // without input until a driver for its own provides some.
    if crate::machine::memory::VIRTIO_MMIO_SLOTS == 0
    {
        serial!("INPUT_ABSENT: this machine declares no virtio transport bank");
    }
    else if devices != 2
    {
        serial!("INPUT_UNAVAILABLE devices={}", devices);
        return Err(recovery_boot_error("native input initialization refused", NOT_SUPPORTED).into());
    }
    else
    {
        serial!("VIRTIO_INPUT=READY devices={}", devices);
    }
    serial!("UI_OBSERVER address={} bytes=128 version=1",
            crate::toolbox::quickdraw::observer::address());
    runtime.boot_progress(&mut boot_screen, 4)?;
    TOOLBOX.publish(runtime)
           .map_err(|_| mac_formats::Error::Malformed("desktop already published"))?;
    with_toolbox(|toolbox| toolbox.boot_progress(&mut boot_screen, 5))
        .map_err(|error| recovery_boot_error("boot progress manager unavailable", error.status()))??;
    serial!("BOOT_SYSTEM_READY: services=0x{:x}", &SERVICES as *const SystemServices as usize);
    // Even applications without an explicit provider requirement must bind
    // their original launch directory before author execution. The unique
    // startup flight owns the same admission while any real job is pending.
    let pending =
        crate::toolbox::processes::startup::install(finder_admission, finder_tls, finder_loaded,
                                                    ready, boot_screen).map_err(|status| {
            recovery_boot_error("Finder pending startup publication refused", status)
        })?;
    Ok(Some(pending))
}

pub(crate) fn finish_initial_finder(finder_admission: &mut crate::toolbox::processes::LaunchAdmission,
                                    finder_loaded: LoadedImage,
                                    boot_screen: &mut crate::boot_screen::BootScreen)
                                    -> crate::loader::LoadResult<()>
{
    serial!("SYSTEM_FINDER_LAUNCH entry=0x{:x}", finder_loaded.entry());
    // Both immediate bootstrap and delayed READY reach this same authentic
    // admission. No Desktop guard or copied owner ID authorizes the page root.
    let finder_space = crate::machine::paging::process::admit(finder_admission.owner(),
                                                              &finder_admission.allocation(),
                                                              &finder_loaded).map_err(|cause| {
                           serial!("PROCESS_SPACE_REFUSED owner={} {cause}", finder_admission.id());
                           recovery_boot_error("Finder space admission refused", cause.status())
                       })?;
    let thread =
        with_toolbox(|toolbox| {
            toolbox.spawn_process(crate::context::finder_app().0, finder_loaded, finder_space, true)
        }).map_err(|_| mac_formats::Error::Malformed("Finder owner directory is unavailable"))?
          .map_err(|status| {
              serial!("FINDER_THREAD_CREATION_FAILED status={status}");
              mac_formats::Error::Malformed("Finder thread creation failed")
          })?;
    finder_admission.started();
    with_toolbox(|toolbox| toolbox.boot_progress(boot_screen, 6))
        .map_err(|error| recovery_boot_error("boot progress manager unavailable", error.status()))??;
    with_toolbox(|toolbox| toolbox.started(crate::context::finder_app().0, thread)).map_err(|_| {
                                                          mac_formats::Error::Malformed("Finder context \
                                                                              initialization \
                                                                              failed")
                                                      })?;
    with_toolbox(|toolbox| toolbox.boot_progress(boot_screen, 7))
        .map_err(|error| recovery_boot_error("boot progress manager unavailable", error.status()))??;
    with_toolbox(|toolbox| toolbox.finish_boot_progress(boot_screen))
        .map_err(|error| recovery_boot_error("boot progress cleanup unavailable", error.status()))??;
    let allocations = crate::allocator::statistics();
    serial!("RESIDENT_ALLOCATION_READY capacity={} admitted={} foundation={} system={} \
             domains={} header_bytes={} owned_app_heap=true module_heap=false \
             isolation_qualification=pending",
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
///
/// The linker script puts `.text.entry` first and the image's entry is read from
/// the ELF, so naming this section is what states the entry rather than leaving
/// it to whatever the linker happens to order first in `.text`. Without it the
/// entry was `kernel::log` on one architecture and `kernel::start` on another,
/// which is a difference no reader would predict.
#[unsafe(link_section = ".text.entry")]
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
        crate::machine::emergency::refuse(format_args!("BOOT_SUPERVISOR_REJECTED: the bootstrap \
                                                        supplied no boot record"));
    }
    let header = unsafe { &*info.cast::<Header>() };
    if !header.supports::<BootInfo>() || header.size as usize != size_of::<BootInfo>()
    {
        // Terminal rather than returning: the caller does not exist, so a return
        // re-enters the same refusal and buries the one message that says why.
        crate::machine::emergency::refuse(format_args!("BOOT_SUPERVISOR_REJECTED: native \
                                                        BootInfo layout is incompatible: \
                                                        version {} size {}, expected version {} \
                                                        size {}; rebuild ROM and System together",
                                                       header.version,
                                                       header.size,
                                                       ABI_VERSION,
                                                       size_of::<BootInfo>()));
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
        // Terminal, and it was not: returning went to an address the loader never
        // set, so one refusal became 358,000 identical lines and the gate reported
        // only that a marker had not arrived.
        crate::machine::emergency::refuse(format_args!("BOOT_SUPERVISOR_REJECTED: {error}"));
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
    // The terminal kit is built on the fw_cfg display and the virtio input pair,
    // so a machine that declares neither window has nothing to build it from.
    // Such a machine still boots graphically; a fatal stop there is reported on
    // the console alone, which is what a missing kit already means.
    if info.flags & mac_abi::BOOT_GRAPHICAL != 0 && crate::machine::memory::FW_CFG_BASE == 0
    {
        crate::machine::emergency::serial(format_args!("EMERGENCY_KIT_ABSENT: this machine \
                                                        declares no fw_cfg display; fatal stops \
                                                        report on the console only"));
    }
    if info.flags & mac_abi::BOOT_GRAPHICAL != 0
       && crate::machine::memory::FW_CFG_BASE != 0
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
    if let Err(error) = crate::machine::platform_devices::retain_inventory()
    {
        serial!("BOOT_DEVICE_INVENTORY_REFUSED: {error}");
        return ptr::null();
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
    // Delivery is brought up before anything can arm a deadline. A timer the
    // machine has nowhere to deliver is a scheduler that never ticks again, so a
    // refused controller is a refused boot rather than a silent wait.
    if let Err(error) = crate::machine::interrupts::admit(info)
    {
        serial!("BOOT_INTERRUPTS_REJECTED: {error}");
        return ptr::null();
    }
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
    // The pooled-resource arbiter is new resident machinery, so its invariants
    // are exercised once here rather than discovered by the first driver that
    // leases a channel. It prints what it observed, so the boot log carries the
    // evidence.
    crate::drivers::leases::self_check();
    serial!("System: native service table ready");
    if info.flags & mac_abi::BOOT_GRAPHICAL == 0
    {
        return &HEADLESS_SERVICES;
    }
    if let Err(error) = launch()
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
