//! Protected service storage is distinct from authority and modal ownership.
//! Only resident recovery services can claim this session. Application requests
//! still validate their ordinary owner and object IDs at the service boundary.
#![forbid(unsafe_code)]
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU64, Ordering};

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
// There is one reusable modal session. It cannot be reopened until every old
// Activity and canonical acknowledgement pin is released. Keep its closing
// flag outside the Arc so the final use can signal after dropping that Arc.
static CLOSING: AtomicBool = AtomicBool::new(false);

const REPORT_PROCESS: u8 = 0;
const REPORT_SYSTEM_LEASED: u8 = 1;
const REPORT_SYSTEM_ABANDONED: u8 = 2;

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
    system_report:  AtomicU8,
    report_power:   Spinlock<Option<Option<(Status, u32)>>>,
    permit:         Permit,
    subject:        Subject,
    publication:    crate::drivers::jobs::RecoveryEndpoints,
    object:         AtomicU64,
    windows:        Spinlock<[(u64, bool); 2]>,
    core_payload:   Spinlock<CorePayloadState>,
    core_frames:    Spinlock<CoreFrames>,
    report_frames:  Spinlock<ReportFrames>,
    file_vectors:   Spinlock<StandardFileVectorsState>,
    cleanup_status: AtomicI32
}

enum StandardFileVectorsState
{
    Idle,
    Borrowed,
    Pending(crate::toolbox::standard_file::Vectors)
}

/// Sole return authority for the pair checked out by this genuine Opening.
#[must_use = "retain the Standard File vector lease with its actual Dialog"]
pub(crate) struct StandardFileVectorsLease
{
    activity: Activity
}
impl StandardFileVectorsLease
{
    pub(crate) fn matches_activity(&self, activity: &Activity) -> bool
    {
        self.activity.same_session(activity)
    }

    pub(crate) fn park(self, vectors: crate::toolbox::standard_file::Vectors)
    {
        // This non-Clone lease is the sole writer to its Borrowed slot. Its
        // Activity excludes reaping until after the inert transfer; the pair
        // contains no Activity/Subject and cannot keep this Session alive.
        InvocationGate::run(|| {
            *self.activity.session.file_vectors.lock() = StandardFileVectorsState::Pending(vectors);
        });
    }
}

/// Produced only after the original Session/Subject/publication and child ACK
/// drain. Refusal retains this Session's same pending pair for a later reap.
pub(crate) struct StandardFileVectorsQuiescence<'a>
{
    session: &'a mut Session
}
impl StandardFileVectorsQuiescence<'_>
{
    pub(crate) fn clear(&mut self) -> Result<bool, Status>
    {
        match self.session.file_vectors.get_mut()
        {
            StandardFileVectorsState::Idle => Ok(false),
            StandardFileVectorsState::Pending(vectors) =>
            {
                vectors.clear();
                Ok(true)
            }
            StandardFileVectorsState::Borrowed => Err(mac_abi::BAD_ABI)
        }
    }

    pub(crate) fn take(self) -> Result<Option<crate::toolbox::standard_file::Vectors>, Status>
    {
        let slot = self.session.file_vectors.get_mut();
        match core::mem::replace(slot, StandardFileVectorsState::Idle)
        {
            StandardFileVectorsState::Idle => Ok(None),
            StandardFileVectorsState::Pending(vectors) => Ok(Some(vectors)),
            StandardFileVectorsState::Borrowed =>
            {
                *slot = StandardFileVectorsState::Borrowed;
                Err(mac_abi::BAD_ABI)
            }
        }
    }
}

enum CorePayloadState
{
    Idle,
    Borrowed,
    Pending(crate::toolbox::dialogs::core_alerts::PendingReturn)
}

struct CoreFrames
{
    window: Option<mac_abi::WindowId>,
    ready:  [Option<crate::toolbox::quickdraw::retained::CoreFrame>; 2]
}
impl CoreFrames
{
    fn unique(&mut self) -> Result<(), Status>
    {
        if self.window.is_none()
        {
            return if self.ready.iter().all(Option::is_none)
            {
                Ok(())
            }
            else
            {
                Err(mac_abi::BAD_ABI)
            };
        }
        for frame in &mut self.ready
        {
            frame.as_mut().ok_or(mac_abi::BAD_ABI)?.unique()?;
        }
        Ok(())
    }
}

/// A real full-scene owner. Its slot is not an object ID; actual Window and
/// Session authority remain pinned until this frame is parked at its origin.
#[must_use = "retain the Core frame lease with its command storage until returning the same frame"]
pub(crate) struct CoreFrameLease
{
    activity: Activity,
    window:   mac_abi::WindowId,
    slot:     usize
}
impl CoreFrameLease
{
    pub(crate) fn matches(&self, activity: &Activity, window: mac_abi::WindowId) -> bool
    {
        self.window == window && self.activity.same_session(activity)
    }

    pub(crate) fn park(self, frame: crate::toolbox::quickdraw::retained::CoreFrame)
    {
        // The affine lease is the sole writer to its absent ready slot. Its
        // Activity excludes reaping/rebinding until AFTER this inert transfer.
        InvocationGate::run(|| {
            self.activity.session.core_frames.lock().ready[self.slot] = Some(frame);
        });
    }
}

/// Four command owners cover the original scene, the latest two disjoint
/// report/warning patches, and one pending update. These inert buffers stay in
/// the original reusable Session; they never borrow the Core frame pool.
struct ReportFrames
{
    window: Option<(mac_abi::WindowId, bool)>,
    limit:  usize,
    ready:  [Option<crate::toolbox::quickdraw::retained::ReportFrame>; 4],
    ports:  [ReportPortSlot; 3]
}
struct ReportPortSlot
{
    port:     Arc<crate::toolbox::quickdraw::service::Port>,
    captured: bool
}
impl ReportPortSlot
{
    fn prepare() -> Result<Self, Status>
    {
        Ok(Self { port: Arc::try_new(crate::toolbox::quickdraw::service::Port::default())
                    .map_err(|_| NO_MEMORY)?, captured: false })
    }
}
/// Roles of the actual native report's three real port transitions.
#[derive(Clone, Copy)]
pub(crate) enum ReportPortKind
{
    Reset,
    Viewport,
    Footer
}
impl ReportPortKind
{
    fn index(self) -> usize
    {
        match self
        {
            Self::Reset => 0,
            Self::Viewport => 1,
            Self::Footer => 2
        }
    }
}
/// Lexical native paint view. Actual Port readers drop before Activity wakes
/// the original closing Session; no mutable Arc or stock slot escapes.
pub(crate) struct ReportPortView
{
    port:     Arc<crate::toolbox::quickdraw::service::Port>,
    activity: Activity,
    window:   mac_abi::WindowId,
    frame:    usize
}
impl ReportFrames
{
    fn prepare(limit: usize, text_count: usize, text_bytes: usize) -> Result<Self, Status>
    {
        use crate::toolbox::quickdraw::retained::ReportFrame;
        Ok(Self { window: None,
                  limit,
                  ready: [Some(ReportFrame::prepare(limit, text_count, text_bytes)?),
                          Some(ReportFrame::prepare(limit, text_count, text_bytes)?),
                          Some(ReportFrame::prepare(limit, text_count, text_bytes)?),
                          Some(ReportFrame::prepare(limit, text_count, text_bytes)?)],
                  ports: [ReportPortSlot::prepare()?,
                          ReportPortSlot::prepare()?,
                          ReportPortSlot::prepare()?] })
    }

    fn returned(&mut self) -> Result<(), Status>
    {
        for frame in &mut self.ready
        {
            frame.as_mut().ok_or(SERVICE_BUSY)?.validate(self.limit)?;
        }
        for slot in &mut self.ports
        {
            Arc::get_mut(&mut slot.port).ok_or(SERVICE_BUSY)?;
        }
        Ok(())
    }

    fn reset_ports(&mut self)
    {
        for slot in &mut self.ports
        {
            slot.captured = false;
        }
    }
}

#[must_use = "retain the report frame lease with its SAME command buffer"]
pub(crate) struct ReportFrameLease
{
    activity: Activity,
    window:   mac_abi::WindowId,
    slot:     usize
}
impl ReportFrameLease
{
    pub(crate) fn matches(&self, activity: &Activity, window: mac_abi::WindowId) -> bool
    {
        self.window == window && self.activity.same_session(activity)
    }

    pub(crate) fn capture_port(&self, activity: &Activity, window: mac_abi::WindowId,
                               kind: ReportPortKind,
                               actual: &crate::toolbox::quickdraw::service::Port)
                               -> Result<ReportPortView, Status>
    {
        if !self.matches(activity, window)
        {
            return Err(ACCESS_DENIED);
        }
        activity.capture_report_port(window, self.slot, kind, actual)
    }

    pub(crate) fn text_port(&self, view: &ReportPortView, window: mac_abi::WindowId,
                            actual: &crate::toolbox::quickdraw::service::Port)
                            -> Result<Arc<crate::toolbox::quickdraw::service::Port>, Status>
    {
        if !self.matches(&view.activity, window)
           || view.window != window
           || view.frame != self.slot
           || view.port.origin != actual.origin
           || view.port.state != actual.state
        {
            return Err(ACCESS_DENIED);
        }
        Ok(view.port.clone())
    }

    pub(crate) fn park(self, frame: crate::toolbox::quickdraw::retained::ReportFrame)
    {
        // The affine lease alone owns this absent slot. Commands destroyed
        // their readers before this transfer; Activity drops/wakes afterwards.
        InvocationGate::run(|| {
            self.activity.session.report_frames.lock().ready[self.slot] = Some(frame);
        });
    }
}

pub(crate) struct QuiescedCore
{
    pub(crate) payload: Option<crate::toolbox::dialogs::core_alerts::PendingReturn>,
    pub(crate) frames:  Option<crate::toolbox::quickdraw::retained::CoreFramePair>
}

/// Affine return route to the exact Opening which checked out Core stock.
/// The Activity lives only in the Prepared/live state, never in parked stock.
#[must_use = "retain the Core payload lease until its semantic state returns the same buffers"]
pub(crate) struct CorePayloadLease
{
    activity: Activity,
    template: usize
}
impl CorePayloadLease
{
    pub(crate) fn matches_activity(&self, activity: &Activity) -> bool
    {
        self.activity.same_session(activity)
    }

    pub(crate) fn template(&self) -> usize
    {
        self.template
    }

    pub(crate) fn park(self, pending: crate::toolbox::dialogs::core_alerts::PendingReturn)
    {
        // Only this non-Clone lease can fill the Borrowed slot. It holds the
        // original Activity until after parking, excluding a later Session use.
        // PendingReturn has no Activity/Subject/owner pin, so there is no cycle.
        InvocationGate::run(|| {
            *self.activity.session.core_payload.lock() = CorePayloadState::Pending(pending);
        });
    }
}

/// Minted only after the exact old Session's original ACK and child drain.
/// It can take only that Session's OWN pending return, never a supplied payload.
#[must_use = "consume the original Session's quiescence proof before making it ready"]
pub(crate) struct CorePayloadQuiescence<'a>
{
    session: &'a mut Session
}
impl CorePayloadQuiescence<'_>
{
    pub(crate) fn has_frames(&mut self) -> bool
    {
        self.session.core_frames.get_mut().window.is_some()
    }

    pub(crate) fn template(&mut self) -> Result<Option<usize>, Status>
    {
        match self.session.core_payload.get_mut()
        {
            CorePayloadState::Idle => Ok(None),
            CorePayloadState::Pending(pending) => Ok(Some(pending.template())),
            CorePayloadState::Borrowed => Err(mac_abi::BAD_ABI)
        }
    }

    pub(crate) fn take(self) -> Result<QuiescedCore, Status>
    {
        match self.session.core_payload.get_mut()
        {
            CorePayloadState::Idle =>
            {}
            CorePayloadState::Borrowed => return Err(mac_abi::BAD_ABI),
            CorePayloadState::Pending(pending) => pending.unique()?
        }
        let frames = self.session.core_frames.get_mut();
        frames.unique()?;
        let window = frames.window;
        let returned_frames = if window.is_some()
        {
            let ready = core::mem::replace(&mut frames.ready, [None, None]);
            match ready
            {
                [Some(a), Some(b)] =>
                {
                    Some(crate::toolbox::quickdraw::retained::CoreFramePair([a, b]))
                }
                other =>
                {
                    frames.ready = other;
                    return Err(mac_abi::BAD_ABI);
                }
            }
        }
        else
        {
            None
        };
        let slot = self.session.core_payload.get_mut();
        match core::mem::replace(slot, CorePayloadState::Idle)
        {
            CorePayloadState::Pending(pending) =>
            {
                self.session.core_frames.get_mut().window = None;
                Ok(QuiescedCore { payload: Some(pending),
                                  frames:  returned_frames })
            }
            CorePayloadState::Idle =>
            {
                self.session.core_frames.get_mut().window = None;
                Ok(QuiescedCore { payload: None,
                                  frames:  returned_frames })
            }
            other =>
            {
                *slot = other;
                if let Some(crate::toolbox::quickdraw::retained::CoreFramePair([a, b])) =
                    returned_frames
                {
                    self.session.core_frames.get_mut().ready = [Some(a), Some(b)];
                }
                Err(mac_abi::BAD_ABI)
            }
        }
    }
}

/// A real use of the current modal service. Child payloads retain this guard
/// through disposal and quarantine. It is neither an issued object ID nor an
/// account selector; public object IDs continue through their canonical stores.
#[derive(Clone)]
pub(crate) struct Activity
{
    session:   Arc<Session>,
    // Fields drop in declaration order: this wake runs after the actual use
    // is gone, even if preempted between Activity::drop and Arc destruction.
    _released: ReleasedActivity
}
/// The sole ownership of a Finder diagnostic which has become a System
/// service. The same Session, Subject, permit and canonical child endpoints
/// survive; no process is fabricated and no stopped owner is revived.
#[must_use = "retain the full System report until its actual dismissal ACK"]
pub(crate) struct SystemReportLease
{
    activity: Activity
}
impl SystemReportLease
{
    pub(crate) fn activity(&self) -> &Activity
    {
        &self.activity
    }

    pub(crate) fn matches_activity(&self, activity: &Activity) -> bool
    {
        self.activity.same_session(activity)
    }
}
impl Drop for SystemReportLease
{
    fn drop(&mut self)
    {
        // Abandonment requests real service cleanup. Child records and the
        // independent System UI slot retain the Session and full report until
        // actual ACK; dropping a lease never resets/reuses live storage.
        self.activity.session.system_report.store(REPORT_SYSTEM_ABANDONED, Ordering::Release);
        self.activity.close();
        crate::toolbox::runtime::system_report_changed();
    }
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

    /// Borrow the actual service storage capability while producing its cheap
    /// reset-lifetime resource proof; no owner Arc is cloned for replay.
    pub(crate) fn resource_source(&self) -> Result<crate::kernel::SystemResources, Status>
    {
        crate::kernel::system_resources(&self.session.storage_owner)
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

    /// Admission for the same genuine Opening before Standard File birth.
    /// Retained origin metadata or a matching recipient ID cannot substitute
    /// for the active Session, live owners and its still-empty protocol slot.
    pub(crate) fn validate_standard_file_begin(&self, recipient: &OwnerRef, storage: &OwnerRef)
                                               -> Result<(), Status>
    {
        self.validate_modal_opening(Kind::StandardFile, recipient, storage)
    }

    pub(crate) fn validate_core_alert_open(&self, recipient: &OwnerRef, storage: &OwnerRef)
                                           -> Result<(), Status>
    {
        self.validate_modal_opening(Kind::CoreAlert, recipient, storage)
    }

    pub(crate) fn lease_standard_file_vectors(&self) -> Result<StandardFileVectorsLease, Status>
    {
        self.validate_standard_file_begin(&self.recipient(), &self.storage_owner())?;
        InvocationGate::run(|| {
            let mut slot = self.session.file_vectors.lock();
            if !matches!(*slot, StandardFileVectorsState::Idle)
            {
                return Err(SERVICE_BUSY);
            }
            *slot = StandardFileVectorsState::Borrowed;
            Ok(StandardFileVectorsLease { activity: self.clone() })
        })
    }

    pub(crate) fn lease_core_payload(&self, template: usize) -> Result<CorePayloadLease, Status>
    {
        self.validate_core_alert_open(&self.recipient(), &self.storage_owner())?;
        InvocationGate::run(|| {
            let mut slot = self.session.core_payload.lock();
            if !matches!(*slot, CorePayloadState::Idle)
            {
                return Err(SERVICE_BUSY);
            }
            *slot = CorePayloadState::Borrowed;
            Ok(CorePayloadLease { activity: self.clone(),
                                  template })
        })
    }

    pub(crate) fn validate_core_paint(&self, window: mac_abi::WindowId) -> Result<(), Status>
    {
        InvocationGate::run(|| {
            let modal = MODAL.lock();
            if !modal.session.as_ref().is_some_and(|session| Arc::ptr_eq(session, &self.session))
               || self.session.kind != Kind::CoreAlert
               || CLOSING.load(Ordering::Acquire)
               || !self.session.recipient.lock().accepting()
               || !self.session.storage_owner.accepting()
               || !self.session
                       .windows
                       .lock()
                       .iter()
                       .any(|(id, closed)| *id == window.0 && !*closed)
            {
                return Err(ACCESS_DENIED);
            }
            Ok(())
        })
    }

    pub(crate) fn bind_core_frames(&self, window: mac_abi::WindowId,
                                   prepare: impl FnOnce() -> Result<crate::toolbox::quickdraw::retained::CoreFramePair, Status>)
                                   -> Result<(), Status>
    {
        self.validate_core_paint(window)?;
        InvocationGate::run(|| {
            let mut frames = self.session.core_frames.lock();
            if let Some(bound) = frames.window
            {
                return if bound == window { Ok(()) } else { Err(ACCESS_DENIED) };
            }
            if frames.ready.iter().any(Option::is_some)
            {
                return Err(mac_abi::BAD_ABI);
            }
            let crate::toolbox::quickdraw::retained::CoreFramePair([a, b]) = prepare()?;
            frames.ready = [Some(a), Some(b)];
            frames.window = Some(window);
            Ok(())
        })
    }

    pub(crate) fn take_core_frame(
        &self, window: mac_abi::WindowId)
        -> Result<(crate::toolbox::quickdraw::retained::CoreFrame, CoreFrameLease), Status>
    {
        self.validate_core_paint(window)?;
        InvocationGate::run(|| {
            let mut frames = self.session.core_frames.lock();
            if frames.window != Some(window)
            {
                return Err(ACCESS_DENIED);
            }
            for (slot, frame) in frames.ready.iter_mut().enumerate()
            {
                if let Some(ready) = frame.as_mut()
                {
                    ready.unique()?;
                    let frame = frame.take().ok_or(mac_abi::BAD_ABI)?;
                    return Ok((frame,
                               CoreFrameLease { activity: self.clone(),
                                                window,
                                                slot }));
                }
            }
            Err(SERVICE_BUSY)
        })
    }

    pub(crate) fn validate_report_paint(&self, window: mac_abi::WindowId) -> Result<(), Status>
    {
        InvocationGate::run(|| {
            let modal = MODAL.lock();
            if !modal.session.as_ref().is_some_and(|session| Arc::ptr_eq(session, &self.session))
               || self.session.kind != Kind::CrashReport
               || CLOSING.load(Ordering::Acquire)
               || !self.session.recipient.lock().accepting()
               || !self.session.storage_owner.accepting()
               || self.session.object.load(Ordering::Acquire) != window.0
               || !self.session
                       .windows
                       .lock()
                       .iter()
                       .any(|(id, closed)| *id == window.0 && !*closed)
            {
                return Err(ACCESS_DENIED);
            }
            Ok(())
        })
    }

    pub(crate) fn bind_report_frames(&self, window: mac_abi::WindowId) -> Result<(), Status>
    {
        self.validate_report_paint(window)?;
        InvocationGate::run(|| {
            let mut frames = self.session.report_frames.lock();
            if let Some((bound, acknowledged)) = frames.window
            {
                if bound == window
                {
                    return if acknowledged { Err(ACCESS_DENIED) } else { Ok(()) };
                }
                if !acknowledged
                {
                    return Err(SERVICE_BUSY);
                }
            }
            // A genuine old Window ACK plus ALL original inert buffers is
            // required before a Finder-to-System same-Session rebind.
            frames.returned()?;
            frames.reset_ports();
            frames.window = Some((window, false));
            Ok(())
        })
    }

    fn capture_report_port(&self, window: mac_abi::WindowId, frame: usize, kind: ReportPortKind,
                           actual: &crate::toolbox::quickdraw::service::Port)
                           -> Result<ReportPortView, Status>
    {
        self.validate_report_paint(window)?;
        InvocationGate::run(|| {
            let mut frames = self.session.report_frames.lock();
            if frames.window != Some((window, false))
            {
                return Err(ACCESS_DENIED);
            }
            if frames.ready.get(frame).ok_or(mac_abi::BAD_ABI)?.is_some()
            {
                return Err(mac_abi::BAD_ABI);
            }
            let slot = &mut frames.ports[kind.index()];
            if slot.captured
            {
                if slot.port.origin != actual.origin || slot.port.state != actual.state
                {
                    return Err(mac_abi::BAD_ABI);
                }
            }
            else
            {
                // Capture the actual post-operation Port. Native report clips
                // are inline rectangles; clone/drop does not allocate here.
                *Arc::get_mut(&mut slot.port).ok_or(SERVICE_BUSY)? = actual.clone();
                slot.captured = true;
            }
            Ok(ReportPortView { port: slot.port.clone(),
                                activity: self.clone(),
                                window,
                                frame })
        })
    }

    pub(crate) fn take_report_frame(
        &self, window: mac_abi::WindowId)
        -> Result<(crate::toolbox::quickdraw::retained::ReportFrame, ReportFrameLease), Status>
    {
        self.validate_report_paint(window)?;
        InvocationGate::run(|| {
            let mut frames = self.session.report_frames.lock();
            if frames.window != Some((window, false))
            {
                return Err(ACCESS_DENIED);
            }
            let limit = frames.limit;
            for (slot, frame) in frames.ready.iter_mut().enumerate()
            {
                if let Some(ready) = frame.as_mut()
                {
                    ready.validate(limit)?;
                    ready.reset_text();
                    let frame = frame.take().ok_or(mac_abi::BAD_ABI)?;
                    return Ok((frame,
                               ReportFrameLease { activity: self.clone(),
                                                  window,
                                                  slot }));
                }
            }
            Err(SERVICE_BUSY)
        })
    }

    fn validate_modal_opening(&self, kind: Kind, recipient: &OwnerRef, storage: &OwnerRef)
                              -> Result<(), Status>
    {
        InvocationGate::run(|| {
            let modal = MODAL.lock();
            if !modal.session.as_ref().is_some_and(|session| Arc::ptr_eq(session, &self.session))
               || self.session.kind != kind
               || self.session.recipient.lock().owner() != recipient.owner()
               || self.owner() != recipient.owner().incarnation()
               || self.session.storage_owner.owner() != storage.owner()
               || recipient.owner().kind() != mac_object_table::domain::DomainKind::Process
               || storage.owner().kind() != mac_object_table::domain::DomainKind::System
            {
                return Err(ACCESS_DENIED);
            }
            if CLOSING.load(Ordering::Acquire)
               || !recipient.accepting()
               || !storage.accepting()
               || self.session.object.load(Ordering::Acquire) != 0
               || self.session.windows.lock().iter().any(|(id, closed)| *id != 0 && !*closed)
            {
                return Err(SERVICE_BUSY);
            }
            Ok(())
        })
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
            let mut frames = self.session.report_frames.lock();
            if let Some((bound, acknowledged)) = frames.window.as_mut()
               && *bound == id
            {
                // This callback is the actual original Window completion ACK,
                // not a later lookup after Session.windows slot reuse.
                *acknowledged = true;
            }
        });
    }

    pub(crate) fn window_close_acknowledged(&self, id: mac_abi::WindowId) -> bool
    {
        InvocationGate::run(|| {
            self.session.windows.lock().iter().any(|(old, closed)| *old == id.0 && *closed)
        })
    }

    pub(crate) fn has_outstanding_windows(&self) -> bool
    {
        InvocationGate::run(|| {
            self.session.windows.lock().iter().any(|(id, closed)| *id != 0 && !*closed)
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

    /// Transfer an already closed Finder report to its genuine System service
    /// owner. Callers must retain the returned affine lease before any later
    /// fallible Finder-role transition. Live child ownership never changes.
    pub(crate) fn retain_as_system_report(&self, previous: &OwnerRef)
                                          -> Result<SystemReportLease, Status>
    {
        let previous = InvocationGate::run(|| {
            let modal = MODAL.lock();
            if !modal.session.as_ref().is_some_and(|session| Arc::ptr_eq(session, &self.session))
               || self.session.kind != Kind::CrashReport
               || CLOSING.load(Ordering::Acquire)
               || self.session.storage_owner.owner().kind()
                  != mac_object_table::domain::DomainKind::System
               || !self.session.storage_owner.accepting()
               || self.session.object.load(Ordering::Acquire) != 0
               || self.session.windows.lock().iter().any(|(id, closed)| *id != 0 && !*closed)
            {
                return Err(ACCESS_DENIED);
            }
            let mut recipient = self.session.recipient.lock();
            if recipient.owner() != previous.owner()
               || self.session.system_report.load(Ordering::Acquire) != REPORT_PROCESS
            {
                return Err(ACCESS_DENIED);
            }
            // The sole active Session and Desktop's actual empty System slot
            // exclude another lease. All refusals precede this transition.
            let old = core::mem::replace(&mut *recipient, self.session.storage_owner.clone());
            self.session
                .recipient_id
                .store(self.session.storage_owner.owner().incarnation(), Ordering::Release);
            self.session.system_report.store(REPORT_SYSTEM_LEASED, Ordering::Release);
            Ok(old)
        })?;
        drop(previous);
        Ok(SystemReportLease { activity: self.clone() })
    }

    pub(crate) fn system_report_power_result(&self, result: Option<(Status, u32)>)
    {
        InvocationGate::run(|| *self.session.report_power.lock() = Some(result));
        crate::toolbox::runtime::system_report_changed();
    }

    pub(crate) fn restore_system_report_power_result(&self, result: Option<(Status, u32)>)
    {
        InvocationGate::run(|| *self.session.report_power.lock() = Some(result));
    }

    pub(crate) fn take_system_report_power_result(&self) -> Option<Option<(Status, u32)>>
    {
        InvocationGate::run(|| self.session.report_power.lock().take())
    }

    pub(crate) fn is_system_report(&self) -> bool
    {
        matches!(self.session.system_report.load(Ordering::Acquire),
                 REPORT_SYSTEM_LEASED | REPORT_SYSTEM_ABANDONED)
    }

    pub(crate) fn system_report_abandoned(&self) -> bool
    {
        self.session.system_report.load(Ordering::Acquire) == REPORT_SYSTEM_ABANDONED
    }

    pub(crate) fn system_report_children_closed(&self) -> bool
    {
        self.system_report_abandoned()
        && self.session.object.load(Ordering::Acquire) == 0
        && !self.has_outstanding_windows()
    }

    pub(crate) fn closing(&self) -> bool
    {
        CLOSING.load(Ordering::Acquire)
    }

    pub(crate) fn close(&self)
    {
        CLOSING.store(true, Ordering::Release);
        CLEANUP_REQUESTED.store(true, Ordering::Release);
        REAP_REQUESTED.store(true, Ordering::Release);
    }

    /// No scope survives return to application code. The System supplies the
    /// service's own authentic I/O root, allocation and backing capabilities.
    pub(crate) fn run<R>(&self, operation: impl FnOnce() -> R) -> Result<R, Status>
    {
        self.with_storage(|| {
                crate::kernel::with_system_resources(&self.session.storage_owner, || {
                    crate::context::with_io_subject(self.subject(), operation)
                }).and_then(core::convert::identity)
            })
    }
}
impl Drop for Activity
{
    fn drop(&mut self)
    {
        if !CLOSING.load(Ordering::Acquire)
           && !InvocationGate::run(|| self.session.recipient.lock().accepting())
        {
            if !CLOSING.swap(true, Ordering::AcqRel)
            {
                CLEANUP_REQUESTED.store(true, Ordering::Release);
                REAP_REQUESTED.store(true, Ordering::Release);
            }
        }
    }
}

#[derive(Clone)]
struct ReleasedActivity;
impl Drop for ReleasedActivity
{
    fn drop(&mut self)
    {
        if CLOSING.load(Ordering::Acquire)
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
            let layout = crate::toolbox::crashes::prepared_layout()?;
            let limit = crate::toolbox::runtime::Toolbox::measure_crash_command_capacity(layout)?;
            let (text_count, text_bytes) =
                crate::toolbox::runtime::Toolbox::measure_crash_text_stock(layout)?;
            let report_frames = ReportFrames::prepare(limit, text_count, text_bytes)?;
            Arc::try_new(Session { recipient: Spinlock::new(owner.clone()),
                                   recipient_id: AtomicU64::new(owner.owner().incarnation()),
                                   storage_owner: owner,
                                   kind: Kind::CoreAlert,
                                   system_report: AtomicU8::new(REPORT_PROCESS),
                                   report_power: Spinlock::new(None),
                                   permit,
                                   subject,
                                   publication,
                                   object: AtomicU64::new(0),
                                   windows: Spinlock::new([(0, true); 2]),
                                   core_payload: Spinlock::new(CorePayloadState::Idle),
                                   core_frames: Spinlock::new(CoreFrames { window: None,
                                                                           ready:  [None, None] }),
                                   report_frames: Spinlock::new(report_frames),
                                   file_vectors: Spinlock::new(StandardFileVectorsState::Idle),
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
    // Returning old stock belongs to pump's guard-free checkpoint. A recursive
    // BEGIN can still have an outer definition/Desktop invocation on its stack.
    // Until the original Session drains there, this request honestly gets Busy.
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
    *unique.system_report.get_mut() = REPORT_PROCESS;
    *unique.report_power.get_mut() = None;
    unique.object.store(0, Ordering::Release);
    *unique.windows.get_mut() = [(0, true); 2];
    CLOSING.store(false, Ordering::Release);
    unique.cleanup_status.store(mac_abi::OK, Ordering::Release);
    InvocationGate::run(|| {
        let mut modal = MODAL.lock();
        modal.opening = false;
        let activity = Activity { session:   session.clone(),
                                  _released: ReleasedActivity };
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
        (session.kind == kind && session.recipient_id.load(Ordering::Acquire) == owner).then(|| {
            Activity { session:   session.clone(),
                       _released: ReleasedActivity }
        })
    })
}

/// Bounded checkpoint: no per-event registry scan or retry loop. Quarantined
/// children keep their real Activity; queued I/O keeps its original Subject.
fn reap()
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
            CLOSING.store(true, Ordering::Release);
        }
        if CLOSING.load(Ordering::Acquire)
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
        let returned = if let Some(unique) = Arc::get_mut(&mut session)
        {
            let file_vectors_idle =
                matches!(unique.file_vectors.get_mut(), StandardFileVectorsState::Idle);
            let core_idle = matches!(unique.core_payload.get_mut(), CorePayloadState::Idle)
                            && unique.core_frames.get_mut().window.is_none()
                            && unique.core_frames.get_mut().ready.iter().all(Option::is_none);
            let returned = if let Err(status) = unique.report_frames.get_mut().returned()
            {
                Err(status)
            }
            else if unique.report_frames.get_mut().window.is_some_and(|(_, ack)| !ack)
            {
                Err(SERVICE_BUSY)
            }
            else if unique.kind == Kind::CoreAlert && file_vectors_idle
            {
                crate::toolbox::dialogs::core_alerts::return_quiesced(CorePayloadQuiescence {
                    session: unique
                })
            }
            else if unique.kind == Kind::StandardFile && core_idle
            {
                crate::toolbox::standard_file::return_vectors(StandardFileVectorsQuiescence {
                    session: unique
                })
            }
            else if core_idle && file_vectors_idle
            {
                Ok(())
            }
            else
            {
                Err(mac_abi::BAD_ABI)
            };
            if returned.is_ok()
            {
                unique.report_frames.get_mut().window = None;
                unique.report_frames.get_mut().reset_ports();
                let owner = unique.storage_owner.clone();
                let old = core::mem::replace(unique.recipient.get_mut(), owner);
                drop(old);
            }
            returned
        }
        else
        {
            Err(SERVICE_BUSY)
        };
        if let Err(status) = returned
        {
            let standard_file = session.kind == Kind::StandardFile;
            let report = session.kind == Kind::CrashReport;
            let previous = session.cleanup_status.swap(status, Ordering::AcqRel);
            InvocationGate::run(|| {
                let mut modal = MODAL.lock();
                // Keep the SAME pending payload and closing Session on refusal.
                modal.session = Some(session);
                modal.opening = false;
                REAP_REQUESTED.store(true, Ordering::Release);
            });
            if previous != status
            {
                if standard_file
                {
                    serial!("STANDARD_FILE_VECTORS_RETURN_REFUSED status={status}; original \
                             closing Session retained");
                }
                else if report
                {
                    serial!("REPORT_COMMANDS_RETURN_REFUSED status={status}; original closing \
                             Session retained");
                }
                else
                {
                    serial!("CORE_PAYLOAD_RETURN_REFUSED status={status}; original closing \
                             Session retained");
                }
            }
            return;
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
    crate::toolbox::runtime::system_report_changed();
    crate::toolbox::crashes::cleanup_progress();
    if ACTIVE.load(Ordering::Acquire)
    {
        CLEANUP_REQUESTED.store(true, Ordering::Release);
        REAP_REQUESTED.store(true, Ordering::Release);
    }
}

pub(crate) fn owner_stopped(owner: &OwnerRef)
{
    // The report may now have a System recipient. Its separate genuine
    // beneficiary stop still wakes the coordinator through the service pump.
    crate::toolbox::runtime::system_report_changed();
    crate::toolbox::crashes::pending_owner_stopped();
    InvocationGate::run(|| {
        if let Some(session) = MODAL.lock().session.as_ref()
           && session.recipient.lock().owner() == owner.owner()
        {
            CLOSING.store(true, Ordering::Release);
            CLEANUP_REQUESTED.store(true, Ordering::Release);
            REAP_REQUESTED.store(true, Ordering::Release);
        }
    });
}

pub(crate) fn claim_report(recipient: OwnerRef) -> Result<Activity, Status>
{
    if !recipient.accepting()
       || recipient.owner().kind() != mac_object_table::domain::DomainKind::Process
    {
        return Err(ACCESS_DENIED);
    }
    if let Some(activity) = active(recipient.owner().incarnation(), Kind::CrashReport)
       && !CLOSING.load(Ordering::Acquire)
    {
        return Ok(activity);
    }
    let opening = begin(recipient, Kind::CrashReport)?;
    let activity = opening.activity().clone();
    opening.commit();
    Ok(activity)
}

/// A fresh accepted Opening proves every previous reusable-session child
/// reached its actual ACK barrier. It pins that exact Session until consumed;
/// no caller-supplied boolean, window lookup or report origin can mint it.
pub(crate) struct FreshReport
{
    activity:  Activity,
    recipient: mac_object_table::domain::DomainId
}
impl FreshReport
{
    pub(crate) fn validate(&self, activity: &Activity, recipient: &OwnerRef) -> Result<(), Status>
    {
        if !self.activity.same_session(activity)
           || self.recipient != recipient.owner()
           || activity.recipient().owner() != self.recipient
        {
            return Err(ACCESS_DENIED);
        }
        if activity.closing()
           || activity.has_outstanding_windows()
           || activity.session.object.load(Ordering::Acquire) != 0
        {
            return Err(SERVICE_BUSY);
        }
        Ok(())
    }
}
pub(crate) struct ReportClaim
{
    activity: Activity,
    fresh:    Option<FreshReport>
}
pub(crate) enum ReportAdoption
{
    Ready,
    FreshClaimNeeded
}
impl ReportClaim
{
    pub(crate) fn into_parts(self) -> (Activity, Option<FreshReport>)
    {
        (self.activity, self.fresh)
    }
}
/// Native failure owns its Process protocol before asking for the one modal.
/// Close only that same reporter's previous dialog; unrelated owners keep it.
pub(crate) fn claim_failure_report(recipient: OwnerRef) -> Result<ReportClaim, Status>
{
    if !recipient.accepting()
       || recipient.owner().kind() != mac_object_table::domain::DomainKind::Process
    {
        return Err(ACCESS_DENIED);
    }
    if let Some(activity) = for_input(recipient.owner().incarnation())
       && activity.session.kind != Kind::CrashReport
    {
        activity.close();
    }
    pump();
    if let Some(activity) = active(recipient.owner().incarnation(), Kind::CrashReport)
       && !CLOSING.load(Ordering::Acquire)
    {
        return Ok(ReportClaim { activity,
                                fresh: None });
    }
    let opening = begin(recipient, Kind::CrashReport)?;
    let activity = opening.activity().clone();
    let fresh = FreshReport { recipient: activity.recipient().owner(),
                              activity:  activity.clone() };
    opening.commit();
    Ok(ReportClaim { activity,
                     fresh: Some(fresh) })
}

/// Failure closes only this owner's previous file/alert protocol. A queued
/// failure must not depend on getting the report runner before its own modal
/// can finish cleanup; the original session/children retain their real ACKs.
pub(crate) fn owner_failed(owner: &OwnerRef)
{
    InvocationGate::run(|| {
        if let Some(session) = MODAL.lock().session.as_ref()
           && session.recipient.lock().owner() == owner.owner()
           && session.kind != Kind::CrashReport
        {
            CLOSING.store(true, Ordering::Release);
            CLEANUP_REQUESTED.store(true, Ordering::Release);
            REAP_REQUESTED.store(true, Ordering::Release);
        }
    });
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
         && !CLOSING.load(Ordering::Acquire)).then(|| Activity { session: session.clone(), _released: ReleasedActivity })
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
            let mut prepared = opening.activity().run(|| {
                crate::toolbox::standard_file::PreparedDialog::prepare(opening.activity(), call)
            })?.map_err(|cause| {
                serial!("STANDARD_FILE_REFUSED: {cause}");
                cause.status()
            })?;
            let result = opening.activity().run(|| {
                                                crate::kernel::with_toolbox(|toolbox| {
                                           toolbox.begin_standard_file(call, &mut prepared)
                                       }).map_err(|error| {
                                             crate::toolbox::standard_file::BeginError::service(
                                                        "Desktop admission", error.status())
                                         })?
                                            })?;
            // Unpublished payload and admission pins are disposed after
            // Desktop release. Published children keep their original cleanup.
            drop(prepared);
            result.map_err(|cause| {
                      serial!("STANDARD_FILE_REFUSED: {cause}");
                      cause.status()
                  })?;
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
                Ok(Activity { session:   session.clone(),
                              _released: ReleasedActivity })
            })?;
            let result = activity.run(|| {
                                     crate::kernel::with_toolbox(|toolbox| {
                                         toolbox.standard_file_request(call)
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
            let mut prepared = opening.activity().run(|| {
                crate::toolbox::dialogs::core_alerts::PreparedAlert::prepare(opening.activity(), call)
            })?.map_err(|cause| {
                serial!("CORE_ALERT_REFUSED: {cause}");
                cause.status()
            })?;
            let result = opening.activity().run(|| {
                crate::kernel::with_toolbox(|toolbox| {
                    toolbox.with_core_dialogs(|toolbox| toolbox.open_prepared_core_alert(call, &mut prepared))
                }).map_err(|error| {
                    crate::toolbox::dialogs::core_alerts::OpenError::service("Desktop admission", error.status())
                })?
            })?;
            // Every unpublished payload remains in this outer preparation;
            // published children retain their original initialization cleanup.
            drop(prepared);
            result.map_err(|cause| {
                      serial!("CORE_ALERT_REFUSED: {cause}");
                      cause.status()
                  })?;
            opening.activity().session.object.store(call.window, Ordering::Release);
            opening.commit();
            return Ok(());
        }
        if is_core && call.operation == VALIDATE_ALERT
        {
            // Validation has no modal Opening or protected allocation right.
            // Instantiate/count using the caller's existing scope, outside Desktop.
            if call.reserved != 0
            {
                return Err(mac_abi::BAD_ARGUMENT);
            }
            call.item = 0;
            let alert = crate::toolbox::dialogs::core_alerts::instantiate(call)?;
            call.item = alert.items.len() as u16;
            return Ok(());
        }
        if !is_core && call.operation == OPEN_ALERT
        {
            if call.reserved != 0
            {
                return Err(mac_abi::BAD_ARGUMENT);
            }
            call.item = 0;
            let selection = crate::kernel::current_resource_selection();
            return crate::context::with_selected_resources(selection, || {
                // Decode once into owned data. No Desktop or source-manager
                // guard crosses real executable preparation/publication waits.
                let alert = crate::kernel::with_resources_in(call.scope, |resources| {
                    crate::toolbox::dialogs::load(resources, call)
                })??;
                call.item = alert.items.len() as u16;
                let buttons = crate::kernel::with_toolbox(|toolbox| {
                    toolbox.preflight_decoded_alert(&owner, &alert)
                }).map_err(|error| error.status())??;
                crate::loader::prepare_definition(*b"WDEF", 0).map_err(crate::loader::status)?;
                if buttons
                {
                    crate::loader::prepare_definition(*b"CDEF", 0).map_err(crate::loader::status)?;
                }
                crate::kernel::with_toolbox(|toolbox| {
                    toolbox.open_prepared_alert(&owner, call, alert)
                }).map_err(|error| error.status())?
            }).map_err(|_| mac_abi::WRONG_THREAD)?;
        }
        let activity = active(owner.owner().incarnation(), Kind::CoreAlert).filter(|activity| {
                                                                               call.window != 0
                    && call.window == activity.session.object.load(Ordering::Acquire)
                                                                           });
        let status = if let Some(activity) = activity
        {
            if call.operation == CLOSE
            {
                if call.reserved != 0
                {
                    return Err(mac_abi::BAD_ARGUMENT);
                }
                // The authenticated close intent survives a refused Desktop
                // entry, including an SDK destructor's discarded Busy result.
                // Completion still waits for the original children and ACKs.
                activity.close();
            }
            let status = activity.run(|| {
                                     crate::kernel::with_toolbox(|toolbox| {
                                         toolbox.with_core_dialogs(|toolbox| toolbox.dialog(call))
                                     }).unwrap_or_else(|error| error.status())
                                 })?;
            status
        }
        else
        {
            crate::kernel::with_toolbox(|toolbox| toolbox.dialog(call)).unwrap_or_else(|error| {
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
    crate::toolbox::runtime::pump_system_report();
    if CLEANUP_REQUESTED.swap(false, Ordering::AcqRel)
    {
        let activity = InvocationGate::run(|| {
            MODAL.lock().session.as_ref().map(|session| {
                                             Activity { session:   session.clone(),
                                                        _released: ReleasedActivity }
                                         })
        });
        if let Some(activity) = activity
        {
            let entered = core::cell::Cell::new(false);
            let result =
                activity.run(|| {
                    crate::kernel::with_toolbox(|toolbox| {
                        entered.set(true);
                        toolbox.windows.retry_recovery_cleanup();
                        if !CLOSING.load(Ordering::Acquire)
                        {
                            return Ok(());
                        }
                        let protocol = match activity.session.kind
                        {
                            Kind::StandardFile =>
                            {
                                toolbox.cancel_standard_file_for_recovery(&activity.recipient())
                            }
                            Kind::CoreAlert => toolbox.cancel_core_alert_for_recovery(&activity),
                            Kind::CrashReport if activity.is_system_report() =>
                            {
                                toolbox.cancel_system_report_for_recovery(&activity)
                            }
                            Kind::CrashReport => toolbox.cancel_crash_report_for_recovery(
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
                            if toolbox.windows.window(id).is_some()
                            {
                                let status = toolbox.window(mac_abi::CLOSE_WINDOW,
                                &mut mac_abi::WindowRequest { id: id.0, bounds: mac_abi::geometry::Rect::ZERO,
                                    title: [0; mac_abi::WINDOW_TITLE_BYTES], title_length: 0,
                                    procedure: 0, minimum_width: 0, minimum_height: 0, reserved: 0 });
                                if status != mac_abi::OK
                                {
                                    failure.get_or_insert(status);
                                }
                            }
                            if toolbox.windows.recovery_close_pending(id, &activity)?
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
        if call.operation != BEGIN && crate::toolbox::runtime::system_report_active()
        {
            let handled = crate::kernel::with_toolbox(|toolbox| {
                              toolbox.system_report_request(&owner, call, input, output)
                          }).map_err(|cause| cause.status())??;
            if handled
            {
                if call.operation == DISMISS
                {
                    pump();
                    let retained = crate::kernel::with_toolbox(|toolbox| {
                                       toolbox.system_report_for_beneficiary(&owner)
                                   }).map_err(|cause| cause.status())??;
                    if retained
                    {
                        return Err(SERVICE_BUSY);
                    }
                    call.info.window = mac_abi::WindowId(0);
                    call.info.dismissed = 1;
                }
                return Ok(());
            }
        }
        match call.operation
        {
            BEGIN | APPEND | INFO | READ =>
            {
                if call.operation == BEGIN
                {
                    crate::kernel::with_toolbox(|toolbox| toolbox.note_crash_capture_attempt())
                        .map_err(|error| error.status())?;
                }
                if call.operation == BEGIN && crate::toolbox::runtime::system_report_active()
                {
                    let closing = crate::kernel::with_toolbox(|toolbox| {
                                      toolbox.request_system_report_close_for(&owner)
                                  }).map_err(|cause| cause.status())??;
                    if closing
                    {
                        pump();
                        let retained = crate::kernel::with_toolbox(|toolbox| {
                                           toolbox.system_report_for_beneficiary(&owner)
                                       }).map_err(|cause| cause.status())??;
                        if retained
                        {
                            return Err(SERVICE_BUSY);
                        }
                    }
                }
                if call.operation == BEGIN
                   && let Some(activity) = active(owner_id, Kind::CrashReport)
                {
                    // Reset is a real service close, including WDEF/child ACK.
                    // Retained old-origin metadata is never used as authority.
                    let closed = activity.run(|| {
                                             crate::kernel::with_toolbox(|toolbox| {
                                                 toolbox.close_crash_report(&activity)
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
                if call.operation == BEGIN
                   && InvocationGate::run(|| {
                       let modal = MODAL.lock();
                       modal.session.as_ref().is_some_and(|session| {
                                                 session.system_report.load(Ordering::Acquire)
                                                 != REPORT_PROCESS
                                                 && CLOSING.load(Ordering::Acquire)
                                             })
                   })
                {
                    // The payload may already be archived while real old
                    // Session/Subject pins are still draining. Never reset or
                    // reuse that modal by treating an empty UI slot as ACK.
                    return Err(SERVICE_BUSY);
                }
                crate::context::with_recovery_allocation(permit()?, || {
                    crate::allocator::with_system(SystemClass::Ordinary, || {
                        crate::kernel::with_toolbox(|toolbox| {
                            toolbox.crash_request(call, input, output)
                        }).map_err(|error| error.status())?
                    })
                })
            }
            SHOW =>
            {
                // Do not let a malformed SHOW cancel a valid, unrelated dialog.
                let info = crate::kernel::with_toolbox(|toolbox| {
                               toolbox.submit_crash_capture(call.flags)
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
                                crate::kernel::with_toolbox(|toolbox| {
                                    toolbox.queue_crash_report(call.flags)
                                }).map_err(|error| error.status())?
                            })
                        })?;
                        return Ok(());
                    }
                    Err(status) => return Err(status)
                };
                let result = activity.run(|| {
                                         crate::kernel::with_toolbox(|toolbox| {
                                             toolbox.crash_request(call, input, output)
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
                            crate::kernel::with_toolbox(|toolbox| {
                                toolbox.crash_request(call, input, output)
                            }).map_err(|error| error.status())?
                        })?
            }
            _ => Err(mac_abi::BAD_ARGUMENT)
        }
    })();
    result.map_or_else(core::convert::identity, |()| mac_abi::OK)
}
