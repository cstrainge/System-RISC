//! Protected service storage is distinct from authority and modal ownership.
//! Only resident recovery services can claim this session. Application requests
//! still validate their ordinary owner and object IDs at the service boundary.
#![forbid(unsafe_code)]
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};

use mac_abi::{ACCESS_DENIED, NO_MEMORY, NOT_FOUND, NOT_SUPPORTED, SERVICE_BUSY, Status};
use mac_object_table::shared::InvocationGate as _;
use spinning_top::Spinlock;

use crate::{allocator::{SystemClass,
                        recovery::{Authority, Permit}},
            drivers::jobs::subjects::Subject,
            toolbox::handles::{InvocationGate, OwnerRef, subject}};

static STORAGE: spin::Once<Authority> = spin::Once::new();
static MODAL: Spinlock<Modal> = Spinlock::new(Modal { opening: false,
                                                      ready:   None,
                                                      session: None });
static REAP_REQUESTED: AtomicBool = AtomicBool::new(false);
static CLEANUP_REQUESTED: AtomicBool = AtomicBool::new(false);
static ACTIVE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind
{
    StandardFile,
    CoreAlert,
    CrashReport
}

struct Modal
{
    opening: bool,
    ready:   Option<Arc<Session>>,
    session: Option<Arc<Session>>
}
struct Session
{
    recipient:      Spinlock<OwnerRef>,
    recipient_id:   AtomicU64,
    storage_owner:  OwnerRef,
    kind:           Kind,
    permit:         Permit,
    subject:        Subject,
    publication:    crate::drivers::jobs::RecoveryEndpoints,
    closing:        AtomicBool,
    object:         AtomicU64,
    windows:        Spinlock<[(u64, bool); 2]>,
    cleanup_status: AtomicI32
}

/// A real use of the current modal service. Child payloads retain this guard
/// through disposal and quarantine. It is neither an issued object ID nor an
/// account selector; public object IDs continue through their canonical stores.
#[derive(Clone)]
pub(crate) struct Activity
{
    session: Arc<Session>
}
impl Activity
{
    pub(crate) fn subject(&self) -> Subject
    {
        Subject::for_recovery(self.clone())
    }

    pub(crate) fn root_subject(&self) -> Subject
    {
        self.session.subject.clone()
    }

    pub(crate) fn owner(&self) -> u64
    {
        self.session.recipient_id.load(Ordering::Acquire)
    }

    pub(crate) fn recipient(&self) -> OwnerRef
    {
        InvocationGate::run(|| self.session.recipient.lock().clone())
    }

    pub(crate) fn storage_owner(&self) -> OwnerRef
    {
        self.session.storage_owner.clone()
    }

    pub(crate) fn publication(&self) -> &crate::drivers::jobs::RecoveryEndpoints
    {
        &self.session.publication
    }

    pub(crate) fn acknowledgement_pin(&self) -> mac_object_table::store::AcknowledgementPin
    {
        self.session.clone()
    }

    pub(crate) fn same_session(&self, other: &Self) -> bool
    {
        Arc::ptr_eq(&self.session, &other.session)
    }

    pub(crate) fn window_created(&self, id: mac_abi::WindowId) -> Result<(), Status>
    {
        InvocationGate::run(|| {
            let mut windows = self.session.windows.lock();
            let slot = windows.iter_mut()
                              .find(|(old, closed)| *old == 0 || *closed)
                              .ok_or(SERVICE_BUSY)?;
            *slot = (id.0, false);
            Ok(())
        })
    }

    pub(crate) fn window_closed(&self, id: mac_abi::WindowId)
    {
        InvocationGate::run(|| {
            if let Some(slot) = self.session.windows.lock().iter_mut().find(|(old, _)| *old == id.0)
            {
                slot.1 = true;
            }
        });
    }

    pub(crate) fn window_close_acknowledged(&self, id: mac_abi::WindowId) -> bool
    {
        InvocationGate::run(|| {
            self.session.windows.lock().iter().any(|(old, closed)| *old == id.0 && *closed)
        })
    }

    /// Restore backing for a resident admission step without replacing its
    /// original request/active-parent receipt or canonical account target.
    pub(crate) fn with_storage<R>(&self, operation: impl FnOnce() -> R) -> R
    {
        crate::context::with_recovery_allocation(self.session.permit.clone(), || {
            crate::context::with_recovery_activity(self.clone(), operation)
        })
    }

    pub(crate) fn set_object(&self, id: u64)
    {
        self.session.object.store(id, Ordering::Release);
    }

    /// System's validated Finder handoff transfers the recipient, never the
    /// System resource owner or the original I/O authority. No caller label can
    /// construct either capability.
    pub(crate) fn handoff_recipient(&self, previous: &OwnerRef, next: &OwnerRef)
                                    -> Result<(), Status>
    {
        if next.owner().kind() != mac_object_table::domain::DomainKind::Process || !next.accepting()
        {
            return Err(ACCESS_DENIED);
        }
        let old = InvocationGate::run(|| {
            let mut recipient = self.session.recipient.lock();
            if recipient.owner() != previous.owner()
            {
                return Err(ACCESS_DENIED);
            }
            let old = core::mem::replace(&mut *recipient, next.clone());
            self.session.recipient_id.store(next.owner().incarnation(), Ordering::Release);
            Ok(old)
        })?;
        drop(old);
        Ok(())
    }

    pub(crate) fn close(&self)
    {
        self.session.closing.store(true, Ordering::Release);
        CLEANUP_REQUESTED.store(true, Ordering::Release);
        REAP_REQUESTED.store(true, Ordering::Release);
    }

    /// No scope survives return to application code. The System supplies the
    /// service's own authentic I/O root, allocation and backing capabilities.
    pub(crate) fn run<R>(&self, operation: impl FnOnce() -> R) -> Result<R, Status>
    {
        self.with_storage(|| crate::context::with_io_subject(self.subject(), operation))
    }
}
impl Drop for Activity
{
    fn drop(&mut self)
    {
        if !self.session.closing.load(Ordering::Acquire)
           && !InvocationGate::run(|| self.session.recipient.lock().accepting())
        {
            if !self.session.closing.swap(true, Ordering::AcqRel)
            {
                CLEANUP_REQUESTED.store(true, Ordering::Release);
                REAP_REQUESTED.store(true, Ordering::Release);
            }
        }
        if self.session.closing.load(Ordering::Acquire) && Arc::strong_count(&self.session) == 2
        {
            REAP_REQUESTED.store(true, Ordering::Release);
        }
    }
}

/// Failed opening releases its claim, while actual child uses remain pinned.
/// Committing transfers modal ownership to the registered service protocol.
pub(crate) struct Opening
{
    activity:  Activity,
    committed: bool
}
impl Opening
{
    pub(crate) fn activity(&self) -> &Activity
    {
        &self.activity
    }

    pub(crate) fn commit(mut self)
    {
        self.committed = true;
    }
}
impl Drop for Opening
{
    fn drop(&mut self)
    {
        if !self.committed
        {
            self.activity.close();
        }
    }
}

/// Called once, after terminal preparation has consumed its separate prefix.
/// The remainder stays reset-pinned; live blocks are never bulk-reclaimed.
pub(crate) fn initialize() -> Result<(), Status>
{
    STORAGE.try_call_once(|| {
               let range =
                   crate::machine::paging::reserve_recovery_storage().map_err(|_| NO_MEMORY)?;
               crate::allocator::recovery::initialize(range).map_err(|_| NO_MEMORY)
           })?;
    Ok(())
}

/// Internal preparation only; never exposed by a trap, caller flag or module
/// Host callback. Resident code must still supply its genuine account target.
pub(crate) fn permit() -> Result<Permit, Status>
{
    Ok(STORAGE.get().ok_or(NOT_SUPPORTED)?.permit())
}

/// Admit the service's actual lifetime root, publication endpoints and reusable
/// session control block during bootstrap. A modal claim never allocates a new
/// DIOP root or attempts to reopen a closed scope. Per-request IDs remain fresh
/// canonical publications through the reserved endpoint capabilities.
pub(crate) fn prepare_modal() -> Result<(), Status>
{
    if InvocationGate::run(|| {
        let modal = MODAL.lock();
        modal.opening || modal.ready.is_some() || modal.session.is_some()
    })
    {
        return Err(mac_abi::BAD_ABI);
    }
    let permit = permit()?;
    // Shared facility indices have ordinary reset-lifetime backing. Reserving
    // real publication capacity now prevents normal traffic consuming it later.
    let (owner, subject, publication) = crate::context::without_recovery(|| {
        crate::allocator::with_system(SystemClass::Ordinary, || {
            let owner = crate::toolbox::handles::system_owner()?;
            let subject = crate::drivers::jobs::subjects::resident()?.admit(
                &subject::Authority::Client(owner.clone()),
                crate::allocation::Target::System(SystemClass::Ordinary))?;
            let publication = crate::drivers::jobs::RecoveryEndpoints::prepare(&owner)?;
            Ok::<_, Status>((owner, subject, publication))
        })
    })?;
    let session = crate::context::with_recovery_allocation(permit.clone(), || {
        crate::allocator::with_system(SystemClass::Ordinary, || {
            Arc::try_new(Session { recipient: Spinlock::new(owner.clone()),
                                   recipient_id: AtomicU64::new(owner.owner().incarnation()),
                                   storage_owner: owner,
                                   kind: Kind::CoreAlert,
                                   permit,
                                   subject,
                                   publication,
                                   closing: AtomicBool::new(false),
                                   object: AtomicU64::new(0),
                                   windows: Spinlock::new([(0, true); 2]),
                                   cleanup_status: AtomicI32::new(mac_abi::OK) }).map_err(|_| {
                                                                                     NO_MEMORY
                                                                                 })
        })
    })?;
    InvocationGate::run(|| MODAL.lock().ready = Some(session));
    Ok(())
}

pub(crate) fn begin(recipient: OwnerRef, kind: Kind) -> Result<Opening, Status>
{
    if recipient.owner().kind() != mac_object_table::domain::DomainKind::Process
       || !recipient.accepting()
    {
        return Err(ACCESS_DENIED);
    }
    reap();
    let mut session = InvocationGate::run(|| {
        let mut modal = MODAL.lock();
        if modal.opening || modal.session.is_some()
        {
            return Err(SERVICE_BUSY);
        }
        modal.opening = true;
        match modal.ready.take()
        {
            Some(session) => Ok(session),
            None =>
            {
                modal.opening = false;
                Err(NOT_SUPPORTED)
            }
        }
    })?;
    let Some(unique) = Arc::get_mut(&mut session)
    else
    {
        // Preserve unexpectedly retained backing; never reset a live session.
        InvocationGate::run(|| {
            let mut modal = MODAL.lock();
            modal.ready = Some(session);
            modal.opening = false;
        });
        return Err(SERVICE_BUSY);
    };
    *unique.recipient.get_mut() = recipient;
    unique.recipient_id.store(unique.recipient.get_mut().owner().incarnation(), Ordering::Release);
    unique.kind = kind;
    unique.object.store(0, Ordering::Release);
    *unique.windows.get_mut() = [(0, true); 2];
    unique.closing.store(false, Ordering::Release);
    unique.cleanup_status.store(mac_abi::OK, Ordering::Release);
    InvocationGate::run(|| {
        let mut modal = MODAL.lock();
        modal.opening = false;
        let activity = Activity { session: session.clone() };
        modal.session = Some(session);
        ACTIVE.store(true, Ordering::Release);
        Ok(Opening { activity,
                     committed: false })
    })
}

pub(crate) fn active(owner: u64, kind: Kind) -> Option<Activity>
{
    if !ACTIVE.load(Ordering::Acquire)
    {
        return None;
    }
    InvocationGate::run(|| {
        let modal = MODAL.lock();
        let session = modal.session.as_ref()?;
        (session.kind == kind && session.recipient_id.load(Ordering::Acquire) == owner)
            .then(|| Activity { session: session.clone() })
    })
}

/// Bounded checkpoint: no per-event registry scan or retry loop. Quarantined
/// children keep their real Activity; queued I/O keeps its original Subject.
pub(crate) fn reap()
{
    if !REAP_REQUESTED.swap(false, Ordering::AcqRel)
    {
        return;
    }
    let removed = InvocationGate::run(|| {
        let mut modal = MODAL.lock();
        let session = modal.session.as_ref()?;
        if !session.recipient.lock().accepting()
        {
            session.closing.store(true, Ordering::Release);
        }
        if session.closing.load(Ordering::Acquire)
           && Arc::strong_count(session) == 1
           && session.subject.exclusive()
           && session.publication.idle()
        {
            modal.opening = true;
            modal.session.take()
        }
        else
        {
            None
        }
    });
    if let Some(mut session) = removed
    {
        if let Some(unique) = Arc::get_mut(&mut session)
        {
            let owner = unique.storage_owner.clone();
            let old = core::mem::replace(unique.recipient.get_mut(), owner);
            drop(old);
        }
        InvocationGate::run(|| {
            let mut modal = MODAL.lock();
            modal.ready = Some(session);
            modal.opening = false;
            ACTIVE.store(false, Ordering::Release);
        });
        crate::toolbox::crashes::modal_available();
    }
}

/// Called by the canonical completion gate after an actual ACK. No lock, scan,
/// logging or allocation is added to the resource-release hot path.
pub(crate) fn acknowledgement_changed()
{
    crate::toolbox::crashes::cleanup_progress();
    if ACTIVE.load(Ordering::Acquire)
    {
        CLEANUP_REQUESTED.store(true, Ordering::Release);
        REAP_REQUESTED.store(true, Ordering::Release);
    }
}

pub(crate) fn owner_stopped(owner: &OwnerRef)
{
    InvocationGate::run(|| {
        if let Some(session) = MODAL.lock().session.as_ref()
           && session.recipient.lock().owner() == owner.owner()
        {
            session.closing.store(true, Ordering::Release);
            CLEANUP_REQUESTED.store(true, Ordering::Release);
            REAP_REQUESTED.store(true, Ordering::Release);
        }
    });
}

pub(crate) fn standard_file_subject(owner: u64) -> Result<Subject, Status>
{
    Ok(for_input(owner).ok_or(NOT_FOUND)?.subject())
}

pub(crate) fn claim_report(recipient: OwnerRef) -> Result<Activity, Status>
{
    if let Some(activity) = active(recipient.owner().incarnation(), Kind::CrashReport)
       && !activity.session.closing.load(Ordering::Acquire)
    {
        return Ok(activity);
    }
    let opening = begin(recipient, Kind::CrashReport)?;
    let activity = opening.activity().clone();
    opening.commit();
    Ok(activity)
}

pub(crate) fn for_input(owner: u64) -> Option<Activity>
{
    if !ACTIVE.load(Ordering::Acquire)
    {
        return None;
    }
    InvocationGate::run(|| {
        let modal = MODAL.lock();
        let session = modal.session.as_ref()?;
        (session.recipient_id.load(Ordering::Acquire) == owner
         && !session.closing.load(Ordering::Acquire)).then(|| Activity { session: session.clone() })
    })
}

pub(crate) fn standard_file_request(call: &mut mac_abi::standard_file::StandardFileCall) -> Status
{
    use mac_abi::standard_file::*;
    let result = (|| {
        let owner = crate::context::current_owner()?;
        if call.operation == BEGIN
        {
            let opening = begin(owner, Kind::StandardFile)?;
            let result = opening.activity().run(|| {
                                                crate::kernel::with_desktop(|desktop| {
                                                    desktop.standard_file_request(call)
                                                }).map_err(|error| error.status())?
                                            })?;
            result?;
            opening.activity().session.object.store(call.handle, Ordering::Release);
            opening.commit();
            Ok(())
        }
        else
        {
            let activity = InvocationGate::run(|| {
                let modal = MODAL.lock();
                let session = modal.session.as_ref().ok_or(NOT_FOUND)?;
                if session.kind != Kind::StandardFile
                   || call.handle == 0
                   || call.handle != session.object.load(Ordering::Acquire)
                {
                    return Err(NOT_FOUND);
                }
                if session.recipient_id.load(Ordering::Acquire) != owner.owner().incarnation()
                {
                    return Err(ACCESS_DENIED);
                }
                Ok(Activity { session: session.clone() })
            })?;
            let result = activity.run(|| {
                                     crate::kernel::with_desktop(|desktop| {
                                         desktop.standard_file_request(call)
                                     }).map_err(|error| error.status())?
                                 })?;
            result
        }
    })();
    result.map_or_else(core::convert::identity, |()| mac_abi::OK)
}

pub(crate) fn dialog_request(call: &mut mac_abi::dialogs::DialogCall) -> Status
{
    use mac_abi::dialogs::*;
    let result = (|| {
        let owner = crate::context::current_owner()?;
        let is_core = crate::toolbox::dialogs::core_alerts::supported(call.scope, call.resource);
        if is_core && call.operation == OPEN_ALERT
        {
            let opening = begin(owner, Kind::CoreAlert)?;
            let status =
                opening.activity().run(|| {
                                       crate::kernel::with_desktop(|desktop| {
                                           desktop.with_core_dialogs(|desktop| desktop.dialog(call))
                                       }).unwrap_or_else(|error| error.status())
                                   })?;
            if status != mac_abi::OK
            {
                return Err(status);
            }
            opening.activity().session.object.store(call.window, Ordering::Release);
            opening.commit();
            return Ok(());
        }
        let activity = active(owner.owner().incarnation(), Kind::CoreAlert).filter(|activity| {
                                                                               call.window != 0
                    && call.window == activity.session.object.load(Ordering::Acquire)
                                                                           });
        let status = if let Some(activity) = activity
        {
            let status = activity.run(|| {
                                     crate::kernel::with_desktop(|desktop| {
                                         desktop.with_core_dialogs(|desktop| desktop.dialog(call))
                                     }).unwrap_or_else(|error| error.status())
                                 })?;
            status
        }
        else
        {
            crate::kernel::with_desktop(|desktop| desktop.dialog(call)).unwrap_or_else(|error| {
                                                                           error.status()
                                                                       })
        };
        if status == mac_abi::OK { Ok(()) } else { Err(status) }
    })();
    result.map_or_else(core::convert::identity, |()| mac_abi::OK)
}


/// Service checkpoint, outside Desktop and module guards. System-owned UI is
/// not in the recipient's teardown walk: cancel its protocol explicitly, then
/// retain the true Window/provider acknowledgements before returning the kit.
pub(crate) fn pump()
{
    if CLEANUP_REQUESTED.swap(false, Ordering::AcqRel)
    {
        let activity = InvocationGate::run(|| {
            MODAL.lock().session.as_ref().map(|session| Activity { session: session.clone() })
        });
        if let Some(activity) = activity
        {
            let entered = core::cell::Cell::new(false);
            let result =
                activity.run(|| {
                    crate::kernel::with_desktop(|desktop| {
                        entered.set(true);
                        desktop.windows.retry_recovery_cleanup();
                        if !activity.session.closing.load(Ordering::Acquire)
                        {
                            return Ok(());
                        }
                        let protocol = match activity.session.kind
                        {
                            Kind::StandardFile =>
                            {
                                desktop.cancel_standard_file_for_recovery(&activity.recipient())
                            }
                            Kind::CoreAlert => desktop.cancel_core_alert_for_recovery(&activity),
                            Kind::CrashReport => desktop.cancel_crash_report_for_recovery(
                                &activity, mac_abi::WindowId(activity.session.object.load(Ordering::Acquire)))
                        };
                        // A failed close can already have invalidated its protocol.
                        // The session still remembers every actually published child.
                        let windows = InvocationGate::run(|| *activity.session.windows.lock());
                        let mut failure = protocol.err();
                        for (id, closed) in windows
                        {
                            if id == 0 || closed
                            {
                                continue;
                            }
                            let id = mac_abi::WindowId(id);
                            if desktop.windows.window(id).is_some()
                            {
                                let status = desktop.window(mac_abi::CLOSE_WINDOW,
                                &mut mac_abi::WindowRequest { id: id.0, bounds: mac_abi::Rect::ZERO,
                                    title: [0; mac_abi::WINDOW_TITLE_BYTES], title_length: 0,
                                    procedure: 0, minimum_width: 0, minimum_height: 0, reserved: 0 });
                                if status != mac_abi::OK
                                {
                                    failure.get_or_insert(status);
                                }
                            }
                            if desktop.windows.recovery_close_pending(id, &activity)?
                            {
                                failure.get_or_insert(SERVICE_BUSY);
                            }
                        }
                        failure.map_or(Ok(()), Err)
                    }).map_err(|error| error.status())?
                })
                .and_then(core::convert::identity);
            let status = result.err().unwrap_or(mac_abi::OK);
            if !entered.get() && status == SERVICE_BUSY
            {
                // Entry contention has not offered any cleanup ticket to a
                // callback. Preserve that admission intent for the next safe
                // service checkpoint; retained WDEF failures still await ACK.
                CLEANUP_REQUESTED.store(true, Ordering::Release);
            }
            let previous = activity.session.cleanup_status.swap(status, Ordering::AcqRel);
            if status != mac_abi::OK && previous != status
            {
                serial!("RECOVERY_MODAL_CLEANUP_RETAINED app={} status={status}; original \
                         children and acknowledgement barrier retained",
                        activity.owner());
            }
        }
    }
    reap();
}

/// Capturing a diagnostic uses the preadmitted System-owned report stock.
/// Showing it additionally claims the one modal service, after cancelling only
/// this reporter's own previous modal protocol. Other apps receive typed Busy.
pub(crate) fn crash_request(call: &mut mac_abi::crashes::CrashCall, input: &str, output: &mut [u8])
                            -> Status
{
    use mac_abi::crashes::*;
    let result = (|| {
        let owner = crate::context::current_owner()?;
        let owner_id = owner.owner().incarnation();
        match call.operation
        {
            BEGIN | APPEND | INFO | READ =>
            {
                if call.operation == BEGIN
                   && let Some(activity) = active(owner_id, Kind::CrashReport)
                {
                    // Reset is a real service close, including WDEF/child ACK.
                    // Retained old-origin metadata is never used as authority.
                    let closed = activity.run(|| {
                                             crate::kernel::with_desktop(|desktop| {
                                                 desktop.close_crash_report(&activity)
                                             }).map_err(|error| error.status())?
                                         })
                                         .and_then(core::convert::identity);
                    activity.close();
                    drop(activity);
                    pump();
                    closed?;
                    if active(owner_id, Kind::CrashReport).is_some()
                    {
                        return Err(SERVICE_BUSY);
                    }
                }
                crate::context::with_recovery_allocation(permit()?, || {
                    crate::allocator::with_system(SystemClass::Ordinary, || {
                        crate::kernel::with_desktop(|desktop| {
                            desktop.crash_request(call, input, output)
                        }).map_err(|error| error.status())?
                    })
                })
            }
            SHOW =>
            {
                // Do not let a malformed SHOW cancel a valid, unrelated dialog.
                let info = crate::kernel::with_desktop(|desktop| {
                               let mut info = CrashCall { operation: INFO,
                                                          ..Default::default() };
                               desktop.crash_request(&mut info, "", &mut [])?;
                               Ok::<_, Status>(info.info)
                           }).map_err(|error| error.status())??;
                if info.app == 0
                {
                    return Err(mac_abi::BAD_ARGUMENT);
                }
                if let Some(activity) = for_input(owner_id)
                   && activity.session.kind != Kind::CrashReport
                {
                    activity.close();
                }
                pump();
                let already_shown = active(owner_id, Kind::CrashReport).is_some_and(|activity| {
                                        activity.session.object.load(Ordering::Acquire) != 0
                                    });
                let activity = match claim_report(owner)
                {
                    Ok(activity) => activity,
                    Err(SERVICE_BUSY)
                        if active(owner_id, Kind::StandardFile).is_some()
                           || active(owner_id, Kind::CoreAlert).is_some() =>
                    {
                        call.info = crate::context::with_recovery_allocation(permit()?, || {
                            crate::allocator::with_system(SystemClass::Ordinary, || {
                                crate::kernel::with_desktop(|desktop| {
                                    desktop.queue_crash_report(call.flags)
                                }).map_err(|error| error.status())?
                            })
                        })?;
                        return Ok(());
                    }
                    Err(status) => return Err(status)
                };
                let result = activity.run(|| {
                                         crate::kernel::with_desktop(|desktop| {
                                             desktop.crash_request(call, input, output)
                                         }).map_err(|error| error.status())?
                                     })?;
                if result.is_err() && !already_shown
                {
                    // Failed initial publication must drain partial children.
                    // A rejected redraw of an existing report instead retains
                    // its valid protocol/window and complete diagnostic.
                    activity.close();
                }
                else if result.is_ok()
                {
                    activity.set_object(call.info.window.0);
                }
                result
            }
            SCROLL | DISMISS =>
            {
                let activity = active(owner_id, Kind::CrashReport).ok_or(NOT_FOUND)?;
                activity.run(|| {
                            crate::kernel::with_desktop(|desktop| {
                                desktop.crash_request(call, input, output)
                            }).map_err(|error| error.status())?
                        })?
            }
            _ => Err(mac_abi::BAD_ARGUMENT)
        }
    })();
    result.map_or_else(core::convert::identity, |()| mac_abi::OK)
}
