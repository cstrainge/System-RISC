//! Initial Finder admission through READY and the original launch directory.
//! The scheduler may enter System idle only with the seal minted by install.
#![forbid(unsafe_code)]
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use mac_abi::*;
use mac_object_table::shared::InvocationGate as _;
use spinning_top::Spinlock;

use super::LaunchAdmission;
use crate::{boot_screen::BootScreen,
            drivers::{OwnedJob, jobs::Failure},
            toolbox::{handles::InvocationGate, native_threads}};

/// Unique bootstrap handoff, with no copied owner number or additional pin.
/// There is no public constructor, Clone or Default implementation.
pub(crate) struct PendingStartup
{
    _seal: ()
}
struct Flight
{
    admission: LaunchAdmission,
    runtime:   native_threads::RuntimeAdmission,
    loaded:    Option<crate::kernel::LoadedImage>,
    ready:     Option<OwnedJob>,
    directory: Option<crate::toolbox::files::local::directory::LaunchDirectory>,
    stage:     Stage,
    screen:    BootScreen,
    failure:   Option<Status>
}
static FLIGHT: Spinlock<Option<Flight>> = Spinlock::new(None);
static WAIT_JOB: AtomicU64 = AtomicU64::new(0);
static WAKE: AtomicBool = AtomicBool::new(false);
static RUNNING: AtomicBool = AtomicBool::new(false);

pub(crate) fn install(admission: LaunchAdmission, tls: native_threads::RootBinding,
                      loaded: crate::kernel::LoadedImage, ready: Option<OwnedJob>,
                      screen: BootScreen)
                      -> Result<PendingStartup, Status>
{
    let runtime =
        native_threads::runtime_admission(&tls, admission.owner()).map_err(|cause| cause.status())?;
    let id = ready.as_ref().map_or(0, OwnedJob::notification_id);
    if ready.is_some() && id == 0
    {
        return Err(BAD_ABI);
    }
    let stage = if ready.is_some() { Stage::Ready } else { Stage::Directory };
    let mut next = Some(Flight { admission,
                                 runtime,
                                 loaded: Some(loaded),
                                 ready,
                                 directory: None,
                                 stage,
                                 screen,
                                 failure: None });
    InvocationGate::run(|| {
        let mut flight = FLIGHT.lock();
        if flight.is_some()
        {
            return Err(SERVICE_BUSY);
        }
        *flight = next.take();
        WAIT_JOB.store(id, Ordering::Release);
        WAKE.store(true, Ordering::Release);
        Ok(())
    })?;
    Ok(PendingStartup { _seal: () })
}

/// The seal alone never authorizes an idle bootstrap without the real flight.
pub(crate) fn pending(_: &PendingStartup) -> bool
{
    InvocationGate::run(|| {
        FLIGHT.lock().as_ref().is_some_and(|flight| {
                                  flight.admission.active
                                  && flight.admission.id() == crate::context::finder_app().0
                                  && flight.runtime.owner().owner()
                                     == flight.admission.owner().owner()
                                  && flight.runtime.accepting()
                                  && flight.failure.is_none()
                              })
    })
}

/// A completion is a wake hint, not an admission or result-delivery capability.
pub(crate) fn ready_changed(job: u64)
{
    if job != 0 && WAIT_JOB.load(Ordering::Acquire) == job
    {
        WAKE.store(true, Ordering::Release);
    }
}

struct Running;
impl Drop for Running
{
    fn drop(&mut self)
    {
        RUNNING.store(false, Ordering::Release);
    }
}

fn retain(flight: Flight)
{
    InvocationGate::run(|| *FLIGHT.lock() = Some(flight));
}

// Startup is reset-only after published boot state, like the synchronous boot
// refusal path. The unique admission/image/account remain owned until reset;
// no account closure or cancellation ACK is fabricated by entering terminal UI.
#[derive(Clone, Copy)]
enum Stage
{
    Ready,
    Directory,
    Executable,
    Author
}

fn directory_cause(status: Status) -> &'static str
{
    match status
    {
        NOT_FOUND => "The original launch directory could not be found.",
        ACCESS_DENIED => "Access to the original launch directory was refused.",
        NO_MEMORY => "Memory for the original launch directory was refused.",
        OPERATION_CANCELLED => "Original launch-directory admission was cancelled.",
        IO_ERROR => "An input or output operation failed.",
        STORAGE_UNAVAILABLE => "The original launch volume is unavailable.",
        SERVICE_BUSY => "The original launch directory is busy.",
        BAD_ABI => "The original launch directory does not match its mounted catalog.",
        _ => "The original launch directory could not be admitted."
    }
}

fn refused(mut flight: Flight, status: Status, cancellation: Option<Status>) -> !
{
    flight.admission.stop();
    WAIT_JOB.store(0, Ordering::Release);
    let owner = flight.admission.id();
    let stage = flight.stage;
    let original = flight.admission.original_catalog();
    retain(flight);
    match stage
    {
        Stage::Ready =>
        {
            serial!("SYSTEM_STARTUP_FAILED: Finder app={owner} required ENTR version=1 READY \
                     refused status={status}; cancellation={cancellation:?}; original boot \
                     resources retained, reset required");
            crate::machine::emergency::enter(crate::emergency::Kind::StartupRefused,
                                             format_args!("Finder required ENTR version 1 READY \
                                                           refused (status {status}). \
                                                           Cancellation refusal: \
                                                           {cancellation:?}. Original boot \
                                                           resources remain retained; restart is \
                                                           required."))
        }
        Stage::Directory =>
        {
            serial!("SYSTEM_STARTUP_DIRECTORY_FAILED: Finder app={owner} volume={} parent={} \
                     status={status}; original boot resources retained, reset required",
                    original.volume,
                    original.parent);
            crate::machine::emergency::enter(crate::emergency::Kind::StartupRefused,
                                             format_args!("Finder could not admit its original \
                                                           launch directory (volume {}, \
                                                           directory {}). {} Status: {status}. \
                                                           Original boot resources remain \
                                                           retained; restart is required.",
                                                          original.volume,
                                                          original.parent,
                                                          directory_cause(status)))
        }
        Stage::Executable =>
        {
            serial!("SYSTEM_STARTUP_EXECUTABLE_FAILED: Finder app={owner} volume={} id={} \
                     status={status}; original boot resources retained, reset required",
                    original.volume,
                    original.id);
            crate::machine::emergency::enter(crate::emergency::Kind::StartupRefused,
                                             format_args!("Finder could not protect its original \
                                                           executable (volume {}, item {}). \
                                                           Admission was refused (status \
                                                           {status}). Original boot resources \
                                                           remain retained; restart is required.",
                                                          original.volume, original.id))
        }
        Stage::Author =>
        {
            serial!("SYSTEM_STARTUP_PUBLICATION_FAILED: Finder app={owner} status={status}; \
                     original boot resources retained, reset required");
            crate::machine::emergency::enter(crate::emergency::Kind::StartupRefused,
                                             format_args!("Finder application startup was \
                                                           refused (status {status}). Original \
                                                           boot resources remain retained; \
                                                           restart is required."))
        }
    }
}

/// One real callback/result turn per actual job wake, on primary maintenance.
/// No startup, Desktop, scheduler or registry guard crosses provider execution.
pub(crate) fn pump()
{
    if !WAKE.load(Ordering::Acquire) || RUNNING.swap(true, Ordering::AcqRel)
    {
        return;
    }
    let _running = Running;
    WAKE.store(false, Ordering::Release);
    let flight = InvocationGate::run(|| FLIGHT.lock().take());
    let Some(mut flight) = flight
    else
    {
        return;
    };
    if let Some(job) = flight.ready.as_mut()
    {
        match job.run_once()
        {
            Ok(None) =>
            {
                retain(flight);
                return;
            }
            Err(status) =>
            {
                if let Some(original) = flight.failure
                {
                    refused(flight, original, Some(status));
                }
                flight.failure = Some(status);
                flight.admission.stop();
                if let Err(cancellation) = job.cancel()
                {
                    refused(flight, status, Some(cancellation));
                }
                WAKE.store(true, Ordering::Release);
                retain(flight);
                return;
            }
            Ok(Some(completed)) =>
            {
                WAIT_JOB.store(0, Ordering::Release);
                if let Some(status) = flight.failure
                {
                    refused(flight, status, None);
                }
                match completed.result
                {
                    Ok(bytes) if bytes.is_empty() => (),
                    Ok(_) => refused(flight, BAD_ABI, None),
                    Err(Failure::Cancelled) => refused(flight, OPERATION_CANCELLED, None),
                    Err(Failure::Service(status)) => refused(flight, status, None)
                }
                drop(flight.ready.take());
            }
        }
    }
    flight.stage = Stage::Directory;
    if flight.directory.is_none()
    {
        match crate::toolbox::files::local::directory::LaunchDirectory::prepare(
            flight.admission.owner(), flight.admission.allocation(),
            flight.admission.original_catalog())
        {
            Ok(directory) =>
            {
                // Arm the actual job hint BEFORE its first callback. This is
                // not authority, and a native synchronous turn has no job ID.
                WAIT_JOB.store(directory.notification_id().unwrap_or(0), Ordering::Release);
                flight.directory = Some(directory);
            }
            Err(status) => refused(flight, status, None)
        }
    }
    match crate::toolbox::files::local::directory::LaunchDirectory::advance(&mut flight.directory)
    {
        Ok(false) =>
        {
            if WAIT_JOB.load(Ordering::Acquire) == 0
            {
                refused(flight, BAD_ABI, None);
            }
            retain(flight);
            return;
        }
        Err(status) => refused(flight, status, None),
        Ok(true) => ()
    }
    WAIT_JOB.store(0, Ordering::Release);
    let original = flight.admission.original_catalog();
    serial!("PROCESS_DIRECTORY_READY app={} volume={} parent={} phase=initial",
            flight.admission.id(),
            original.volume,
            original.parent);
    flight.stage = Stage::Executable;
    if let Err(status) = super::pin_launch_executable(flight.admission.owner(),
                                                      flight.admission.allocation(),
                                                      original)
    {
        refused(flight, status, None);
    }
    serial!("PROCESS_EXECUTABLE_READY app={} volume={} id={} phase=initial",
            flight.admission.id(),
            original.volume,
            original.id);
    flight.stage = Stage::Author;
    let Some(loaded) = flight.loaded.take()
    else
    {
        refused(flight, BAD_ABI, None)
    };
    if let Err(error) =
        crate::kernel::finish_initial_finder(&mut flight.admission, loaded, &mut flight.screen)
    {
        let status = crate::loader::status(error);
        refused(flight, status, None);
    }
    // Actual THRD/Process roots now own the author and canonical directory.
}
