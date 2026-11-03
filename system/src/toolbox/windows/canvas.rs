//! Original WIND target exclusion, independent of the author definition family.
//! Headers are prepaid at real Window admission (or original recovery stock).
#![forbid(unsafe_code)]
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use mac_abi::{ACCESS_DENIED, BAD_ABI, NOT_FOUND, SERVICE_BUSY, Status};
use mac_object_table::domain::DomainId;
use spinning_top::Spinlock;

use super::*;
use crate::toolbox::resources::definitions::callbacks::Notice;

const IDLE: u8 = 0;
const CANVAS: u8 = 1;
const GEOMETRY: u8 = 2;
const CLOSING: u8 = 3;

/// Inline in the SAME original close header. The notice belongs to the actual
/// current painter; it carries no definition, control or completion authority.
pub(super) struct CanvasState
{
    mode:      AtomicU8,
    cancelled: AtomicBool,
    notice:    Spinlock<Option<Arc<Notice>>>
}
impl CanvasState
{
    pub(super) fn new() -> Self
    {
        Self { mode:      AtomicU8::new(IDLE),
               cancelled: AtomicBool::new(false),
               notice:    Spinlock::new(None) }
    }

    fn cancel(&self)
    {
        InvocationGate::run(|| {
            if self.mode.load(Ordering::Acquire) == CANVAS
               && !self.cancelled.swap(true, Ordering::AcqRel)
               && let Some(notice) = self.notice.lock().as_ref()
            {
                notice.request();
            }
        });
    }

    pub(super) fn exclude_canvas(&self) -> Result<(), Status>
    {
        if self.mode.load(Ordering::Acquire) == CANVAS
        {
            self.cancel();
            return Err(SERVICE_BUSY);
        }
        Ok(())
    }

    pub(super) fn preflight_close(&self) -> Result<(), Status>
    {
        self.exclude_canvas()?;
        if !matches!(self.mode.load(Ordering::Acquire), IDLE | CLOSING)
        {
            return Err(SERVICE_BUSY);
        }
        Ok(())
    }

    /// Same Desktop turn, only AFTER canonical begin_release succeeds. A
    /// refused owner invalidation must not strand a still-live target closed.
    pub(super) fn closed(&self)
    {
        self.mode.store(CLOSING, Ordering::Release);
    }

    pub(super) fn close(&self) -> Result<(), Status>
    {
        self.preflight_close()?;
        self.closed();
        Ok(())
    }

    /// Only the original idle/ACKed+UI-done+unique recovery header may reset.
    pub(super) fn reset(&self) -> Result<(), Status>
    {
        if matches!(self.mode.load(Ordering::Acquire), CANVAS | GEOMETRY)
        {
            return Err(SERVICE_BUSY);
        }
        self.mode.store(IDLE, Ordering::Release);
        self.cancelled.store(false, Ordering::Release);
        Ok(())
    }
}
#[derive(Clone)]
enum Header
{
    User(Arc<operations::WindowJob>),
    Trusted(Arc<trusted::TrustedCloseHeader>)
}
/// Target lifetime only. This cannot authorize any WDEF/CDEF/SCBX invocation.
pub(crate) struct WindowCanvasGuardian
{
    id:       WindowId,
    storage:  DomainId,
    header:   Header,
    returned: ReturnEdge
}
struct ReturnEdge(Option<Arc<Notice>>);
impl Drop for ReturnEdge
{
    fn drop(&mut self)
    {
        if let Some(notice) = &self.0
        {
            // Original header/source/target readers were released first.
            crate::kernel::window_sources_changed();
            notice.request();
        }
    }
}
impl Clone for WindowCanvasGuardian
{
    fn clone(&self) -> Self
    {
        Self { id:       self.id,
               storage:  self.storage,
               header:   self.header.clone(),
               returned: ReturnEdge(None) }
    }
}
impl Drop for WindowCanvasGuardian
{
    fn drop(&mut self)
    {
        if let Header::Trusted(header) = &self.header
        {
            self.returned.0 = header.canvas_return_notice();
        }
    }
}
impl WindowCanvasGuardian
{
    pub(super) fn user(job: Arc<operations::WindowJob>) -> Self
    {
        Self { id:       job.window_id(),
               storage:  job.storage_owner(),
               header:   Header::User(job),
               returned: ReturnEdge(None) }
    }

    pub(super) fn trusted(header: Arc<trusted::TrustedCloseHeader>, id: WindowId,
                          storage: DomainId)
                          -> Self
    {
        Self { id,
               storage,
               header: Header::Trusted(header),
               returned: ReturnEdge(None) }
    }

    fn state(&self) -> &CanvasState
    {
        match &self.header
        {
            Header::User(job) => &job.canvas,
            Header::Trusted(header) => &header.canvas
        }
    }

    fn same(&self, other: &Self) -> bool
    {
        self.id == other.id
        && self.storage == other.storage
        && match (&self.header, &other.header)
        {
            (Header::User(a), Header::User(b)) => Arc::ptr_eq(a, b),
            (Header::Trusted(a), Header::Trusted(b)) => Arc::ptr_eq(a, b),
            _ => false
        }
    }

    pub(crate) fn window_id(&self) -> WindowId
    {
        self.id
    }

    pub(crate) fn storage_owner(&self) -> DomainId
    {
        self.storage
    }

    pub(crate) fn cancelled(&self) -> bool
    {
        self.state().cancelled.load(Ordering::Acquire)
    }

    pub(crate) fn cancel(&self)
    {
        self.state().cancel();
    }

    pub(super) fn exclude_canvas(&self) -> Result<(), Status>
    {
        self.state().exclude_canvas()
    }

    pub(super) fn close(&self) -> Result<(), Status>
    {
        self.state().close()
    }

    pub(super) fn preflight_geometry(&self) -> Result<(), Status>
    {
        self.state().exclude_canvas()?;
        if self.state().mode.load(Ordering::Acquire) != IDLE
        {
            return Err(SERVICE_BUSY);
        }
        Ok(())
    }

    pub(super) fn geometry(&self) -> Result<GeometryClaim, Status>
    {
        self.state().exclude_canvas()?;
        self.state()
            .mode
            .compare_exchange(IDLE, GEOMETRY, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| SERVICE_BUSY)?;
        Ok(GeometryClaim(self.clone(), ReturnEdge(None)))
    }

    pub(super) fn geometry_active(&self) -> bool
    {
        self.state().mode.load(Ordering::Acquire) == GEOMETRY
    }
}
/// Keep this in the canonical family owner AFTER its source/packet/readers.
/// Drop is valid only after actual RETURN/stopped (or never-enqueued failure).
/// The manager cannot derive author completion merely from this target lease.
#[must_use = "retain the target claim until the actual author and readers drain"]
pub(crate) struct CanvasClaim
{
    guardian: WindowCanvasGuardian,
    returned: ReturnEdge
}
impl CanvasClaim
{
    pub(crate) fn window_id(&self) -> WindowId
    {
        self.guardian.id
    }

    /// Original full storage domain retained by the same target guardian.
    /// This is metadata for family admission, never author/source authority.
    pub(crate) fn storage_owner(&self) -> DomainId
    {
        self.guardian.storage
    }

    pub(crate) fn cancelled(&self) -> bool
    {
        self.guardian.state().cancelled.load(Ordering::Acquire)
    }

    pub(crate) fn cancel(&self)
    {
        self.guardian.state().cancel();
    }
}
impl CanvasClaim
{
    /// Compare the observer retained by this genuine claim; this does not
    /// substitute for Window lifetime or semantic-content validation.
    pub(crate) fn observes(&self, notice: &Arc<Notice>) -> bool
    {
        InvocationGate::run(|| {
            self.guardian
                .state()
                .notice
                .lock()
                .as_ref()
                .is_some_and(|actual| Arc::ptr_eq(actual, notice))
        })
    }
}
impl Drop for CanvasClaim
{
    fn drop(&mut self)
    {
        let notice = InvocationGate::run(|| {
            let state = self.guardian.state();
            let notice = state.notice.lock().take();
            state.mode.store(IDLE, Ordering::Release);
            notice
        });
        // Readers precede this field in its canonical family owner. Notify only
        // actual claim return, never a retry of an unchanged refusal.
        self.returned.0 = notice;
    }
}
pub(super) struct GeometryClaim(WindowCanvasGuardian, ReturnEdge);
impl Drop for GeometryClaim
{
    fn drop(&mut self)
    {
        self.0.state().mode.store(IDLE, Ordering::Release);
        self.1.0 = Some(match &self.0.header
                        {
                            Header::User(job) => job.canvas_return_notice(),
                            Header::Trusted(header) => header.work_notice()
                        });
    }
}
impl OwnedWindow
{
    pub(super) fn canvas_guardian(&self) -> Result<WindowCanvasGuardian, Status>
    {
        match (&self.cleanup_job, &self.trusted_close)
        {
            (Some(job), None) => Ok(WindowCanvasGuardian::user(job.clone())),
            (None, Some(handle)) => Ok(handle.canvas_guardian()),
            _ => Err(BAD_ABI)
        }
    }
}
impl WindowManager
{
    /// Mint from the actual live WIND; no user definition or family source is
    /// selected here. The caller retains its own genuine family authority.
    pub(crate) fn canvas_guardian(&self, id: WindowId) -> Result<WindowCanvasGuardian, Status>
    {
        let (storage, owned) = self.windows
                                   .get_with_owner(id.0)
                                   .filter(|(_, owned)| owned.initialized)
                                   .ok_or(NOT_FOUND)?;
        if !owned.binding.matches(owned.entry)
        {
            return Err(BAD_ABI);
        }
        let guardian = owned.canvas_guardian()?;
        if guardian.id != id || guardian.storage != storage
        {
            return Err(ACCESS_DENIED);
        }
        Ok(guardian)
    }

    /// Call under Desktop immediately before admitting the family-owned job.
    /// A cached guardian cannot admit against a released/rebound WIND.
    pub(crate) fn claim_canvas(&self, guardian: &WindowCanvasGuardian, notice: Arc<Notice>)
                               -> Result<CanvasClaim, Status>
    {
        if !self.canvas_guardian(guardian.id)?.same(guardian)
        {
            return Err(ACCESS_DENIED);
        }
        InvocationGate::run(|| {
            let state = guardian.state();
            let mut target = state.notice.lock();
            state.mode
                 .compare_exchange(IDLE, CANVAS, Ordering::AcqRel, Ordering::Acquire)
                 .map_err(|_| SERVICE_BUSY)?;
            state.cancelled.store(false, Ordering::Release);
            *target = Some(notice);
            Ok(CanvasClaim { guardian: guardian.clone(),
                             returned: ReturnEdge(None) })
        })
    }

    /// Pure readiness projection for an already-retained original target. It
    /// reserves nothing; claim_canvas repeats the actual admission checks.
    pub(crate) fn canvas_available(&self, held: &WindowCanvasGuardian) -> Result<bool, Status>
    {
        let current = self.canvas_guardian(held.id)?;
        if !current.same(held)
        {
            return Err(ACCESS_DENIED);
        }
        Ok(held.state().mode.load(Ordering::Acquire) == IDLE)
    }

    pub(crate) fn validate_canvas_claim(&self, claim: &CanvasClaim) -> Result<(), Status>
    {
        let guardian = self.canvas_guardian(claim.guardian.id)?;
        if !guardian.same(&claim.guardian)
        {
            return Err(ACCESS_DENIED);
        }
        if guardian.state().mode.load(Ordering::Acquire) != CANVAS || claim.cancelled()
        {
            return Err(SERVICE_BUSY);
        }
        Ok(())
    }
}
