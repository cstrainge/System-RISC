//! Desktop admission and real availability edges for suspended Window work.
//! A failed try never reports progress; only a released successful borrow wakes
//! an armed contender. No callback, allocation or scheduling runs under Desktop.
#![forbid(unsafe_code)]
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use spin::Once;

use crate::{state::{AccessError, TryState},
            toolbox::{controls::objects::ControlHost, resources::definitions::callbacks::Notice,
                      runtime::Toolbox}};

pub(super) struct Desktop
{
    value:           TryState<Toolbox>,
    returned_waiter: AtomicBool,
    control_notice:  Once<Arc<Notice>>,
    notice:          Once<Arc<Notice>>
}
impl Desktop
{
    pub(super) const fn new() -> Self
    {
        Self { value:           TryState::new(),
               returned_waiter: AtomicBool::new(false),
               control_notice:  Once::new(),
               notice:          Once::new() }
    }

    pub(super) fn publish(&self, mut value: Toolbox) -> Result<(), AccessError>
    {
        let notice = value.windows.operation_notice();
        let control_notice = value.managed_controls().disposal_notice();
        self.value.publish(value)?;
        self.notice.call_once(|| notice);
        self.control_notice.call_once(|| control_notice);
        Ok(())
    }

    pub(super) fn with<R>(&self, operation: impl FnOnce(&mut Toolbox) -> R)
                          -> Result<R, AccessError>
    {
        self.with_owned((), |(), toolbox| operation(toolbox)).map_err(|(cause, ())| cause)
    }

    pub(super) fn with_owned<A, R>(&self, value: A, operation: impl FnOnce(A, &mut Toolbox) -> R)
                                   -> Result<R, (AccessError, A)>
    {
        let result = self.value.with_owned(value, operation);
        // TryState has already dropped its actual exclusive guard here.
        if result.is_ok()
           && self.returned_waiter.load(Ordering::Acquire)
           && self.returned_waiter.swap(false, Ordering::AcqRel)
        {
            RETURN_EPOCH.try_update(Ordering::AcqRel, Ordering::Acquire, |epoch| {
                            epoch.checked_add(1)
                        })
                        .ok();
            DESKTOP_RETURNED.store(true, Ordering::Release);
            source_returned();
        }
        result
    }

    /// The retry closes release-before-arm without waiting for a holder or
    /// dropping/reconstructing the caller's original affine capability.
    pub(super) fn with_window_owned<A, R>(&self, value: A,
                                          operation: impl FnOnce(A, &mut Toolbox) -> R)
                                          -> Result<R, (AccessError, A)>
    {
        match self.with_owned((value, operation), |(value, operation), toolbox| {
                      operation(value, toolbox)
                  })
        {
            Ok(result) => Ok(result),
            Err((AccessError::Busy, (value, operation))) =>
            {
                self.returned_waiter.store(true, Ordering::Release);
                self.with_owned((value, operation), |(value, operation), toolbox| {
                        operation(value, toolbox)
                    })
                    .map_err(|(cause, (value, _operation))| (cause, value))
            }
            Err((cause, (value, _operation))) => Err((cause, value))
        }
    }

    pub(super) fn work_ready(&self) -> bool
    {
        self.notice.get().is_some_and(|notice| notice.ready())
    }

    pub(super) fn control_work_ready(&self) -> bool
    {
        self.control_notice.get().is_some_and(|notice| notice.ready())
    }

    pub(super) fn notify_control(&self)
    {
        if let Some(notice) = self.control_notice.get()
        {
            notice.request();
        }
    }

    pub(super) fn notify(&self)
    {
        if let Some(notice) = self.control_notice.get()
        {
            notice.request();
        }
        if let Some(notice) = self.notice.get()
        {
            notice.request();
        }
    }
}

static CONTROL_SOURCE_RETURNED: AtomicBool = AtomicBool::new(false);
static SOURCE_RETURNED: AtomicBool = AtomicBool::new(false);

/// Called only after a genuine source thread/manager claim/payload is released.
/// AccountUse/reference disposal is deliberately not projected onto this edge.
pub(crate) fn source_returned()
{
    SOURCE_RETURNED.store(true, Ordering::Release);
    CONTROL_SOURCE_RETURNED.store(true, Ordering::Release);
    super::TOOLBOX.notify();
}

pub(super) fn take_source_returned() -> bool
{
    SOURCE_RETURNED.swap(false, Ordering::AcqRel)
}

/// A revision of actual Desktop availability, never an object/owner identity.
/// Saturation is terminal for this handshake: no old revision can alias reuse.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Epoch(usize);
static RETURN_EPOCH: AtomicUsize = AtomicUsize::new(1);
static DESKTOP_RETURNED: AtomicBool = AtomicBool::new(false);

pub(crate) fn epoch() -> Epoch
{
    Epoch(RETURN_EPOCH.load(Ordering::Acquire))
}
impl Epoch
{
    pub(crate) fn unchanged(self) -> Result<bool, mac_abi::Status>
    {
        let current = RETURN_EPOCH.load(Ordering::Acquire);
        if self.0 == usize::MAX || current == usize::MAX
        {
            return Err(mac_abi::BAD_ABI);
        }
        Ok(self.0 == current)
    }
}
pub(super) fn take_desktop_returned() -> bool
{
    DESKTOP_RETURNED.load(Ordering::Acquire) && DESKTOP_RETURNED.swap(false, Ordering::AcqRel)
}

pub(super) fn take_control_source_returned() -> bool
{
    CONTROL_SOURCE_RETURNED.swap(false, Ordering::AcqRel)
}

static CONTROL_ACKNOWLEDGED: AtomicBool = AtomicBool::new(false);
static WAITING_CONTROL_ACK: AtomicBool = AtomicBool::new(false);
/// Arm before the caller's second original-key ledger check. This is a ready
/// hint registration, never authority or evidence that any ACK has occurred.
pub(crate) fn arm_control_acknowledgement()
{
    WAITING_CONTROL_ACK.store(true, Ordering::Release);
}
/// Only the canonical resource_released hook calls this after real pool ACK.
/// The ordinary no-waiter path performs one load and does not wake maintenance.
/// No manager scan, allocator, scheduler lock or user callback runs here.
pub(crate) fn control_resource_acknowledged()
{
    if WAITING_CONTROL_ACK.load(Ordering::Acquire)
       && WAITING_CONTROL_ACK.swap(false, Ordering::AcqRel)
    {
        CONTROL_ACKNOWLEDGED.store(true, Ordering::Release);
        super::TOOLBOX.notify_control();
    }
}
pub(super) fn take_control_acknowledged() -> bool
{
    CONTROL_ACKNOWLEDGED.swap(false, Ordering::AcqRel)
}
