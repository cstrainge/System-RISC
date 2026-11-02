//! The genuine initial Finder admission while its exact provider is pending.
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
    ready:     OwnedJob,
    screen:    BootScreen,
    failure:   Option<Status>
}
static FLIGHT: Spinlock<Option<Flight>> = Spinlock::new(None);
static READY_JOB: AtomicU64 = AtomicU64::new(0);
static WAKE: AtomicBool = AtomicBool::new(false);
static RUNNING: AtomicBool = AtomicBool::new(false);

pub(crate) fn install(admission: LaunchAdmission, tls: native_threads::RootBinding,
                      loaded: crate::kernel::LoadedImage, ready: OwnedJob, screen: BootScreen)
                      -> Result<PendingStartup, Status>
{
    let runtime =
        native_threads::runtime_admission(&tls, admission.owner()).map_err(|cause| cause.status())?;
    let id = ready.notification_id();
    let mut next = Some(Flight { admission,
                                 runtime,
                                 loaded: Some(loaded),
                                 ready,
                                 screen,
                                 failure: None });
    InvocationGate::run(|| {
        let mut flight = FLIGHT.lock();
        if flight.is_some() || id == 0
        {
            return Err(SERVICE_BUSY);
        }
        *flight = next.take();
        READY_JOB.store(id, Ordering::Release);
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
    if job != 0 && READY_JOB.load(Ordering::Acquire) == job
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
fn refused(mut flight: Flight, status: Status, cancellation: Option<Status>) -> !
{
    flight.admission.stop();
    let owner = flight.admission.id();
    retain(flight);
    serial!("SYSTEM_STARTUP_FAILED: Finder app={owner} required ENTR version=1 READY refused \
             status={status}; cancellation={cancellation:?}; original boot resources retained, \
             reset required");
    crate::machine::emergency::enter(crate::emergency::Kind::StartupRefused,
                                     format_args!("Finder required ENTR version 1 READY refused \
                                                   (status {status}). Cancellation refusal: \
                                                   {cancellation:?}. Original boot resources \
                                                   remain retained; restart is required."))
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
    match flight.ready.run_once()
    {
        Ok(None) => retain(flight),
        Err(status) =>
        {
            if flight.failure.is_some()
            {
                // Result delivery/cancellation is still refused. Keep its
                // actual owned job/root; reset cannot masquerade as cleanup.
                let original = flight.failure.unwrap_or(status);
                refused(flight, original, Some(status));
            }
            flight.failure = Some(status);
            flight.admission.stop();
            if let Err(cancellation) = flight.ready.cancel()
            {
                refused(flight, status, Some(cancellation));
            }
            // Cancellation may itself have completed synchronously. Keep a
            // first cancellation turn; further turns need actual completion.
            WAKE.store(true, Ordering::Release);
            retain(flight);
        }
        Ok(Some(completed)) =>
        {
            READY_JOB.store(0, Ordering::Release);
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
            let Some(loaded) = flight.loaded.take()
            else
            {
                refused(flight, BAD_ABI, None)
            };
            if let Err(error) = crate::kernel::finish_initial_finder(&mut flight.admission, loaded,
                                                                     &mut flight.screen)
            {
                let status = crate::loader::status(error);
                refused(flight, status, None);
            }
            // The genuine published THRD/Process roots now own the application.
            // All bootstrap-only Root/account guards drop on this returning turn.
        }
    }
}
