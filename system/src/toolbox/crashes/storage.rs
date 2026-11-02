//! Full report storage. The native recovery slot is boot-admitted under System
//! authority; immutable snapshots share it without retaining the dying account.
#![forbid(unsafe_code)]
use alloc::{string::String, sync::Arc, vec::Vec};

use mac_abi::{BAD_ABI, NO_MEMORY, NOT_SUPPORTED, SERVICE_BUSY, Status, crashes::MAX_REPORT_BYTES};
use mac_object_table::shared::InvocationGate as _;
use spinning_top::Spinlock;

use crate::toolbox::handles::InvocationGate;

/// Existing canonical process/thread metadata of an accepted capture. Only the
/// sealed manager can retain this key; it is not a new object identity issuer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Origin
{
    app:    u64,
    thread: u64
}
impl Origin
{
    pub(super) fn app(self) -> u64
    {
        self.app
    }
}
pub(super) struct Data
{
    pub(super) report: String,
    pub(super) starts: Vec<u32>,
    origin:            Option<Origin>
}
impl Data
{
    fn empty() -> Self
    {
        Self { report: String::new(),
               starts: Vec::new(),
               origin: None }
    }

    pub(super) fn begin_origin(&mut self, app: u64, thread: u64)
    {
        self.origin = Some(Origin { app,
                                    thread });
    }
}
struct Pool
{
    initialized: bool,
    archive:     Option<Arc<Data>>
}
static POOL: Spinlock<Pool> = Spinlock::new(Pool { initialized: false,
                                                   archive:     None });
pub(super) fn prepare() -> Result<(), Status>
{
    if InvocationGate::run(|| POOL.lock().initialized)
    {
        return Err(BAD_ABI);
    }
    let mut data = Data::empty();
    data.report.try_reserve_exact(MAX_REPORT_BYTES).map_err(|_| NO_MEMORY)?;
    data.starts.try_reserve_exact(MAX_REPORT_BYTES + 1).map_err(|_| NO_MEMORY)?;
    let data = Arc::try_new(data).map_err(|_| NO_MEMORY)?;
    InvocationGate::run(|| {
        let mut pool = POOL.lock();
        if pool.initialized
        {
            return Err(BAD_ABI);
        }
        pool.archive = Some(data);
        pool.initialized = true;
        Ok(())
    })
}
/// Availability is a scheduling hint, not capture authority. The actual claim
/// still validates unique ownership under this same stock gate.
pub(super) fn available() -> bool
{
    InvocationGate::run(|| {
        POOL.lock().archive.as_ref().is_some_and(|data| Arc::strong_count(data) == 1)
    })
}

/// A temporary read pin. Pool metadata is unlocked before callers inspect or
/// copy bytes, and the original account is never retained by this view.
#[derive(Clone)]
struct ReleasedStock;
impl Drop for ReleasedStock
{
    fn drop(&mut self)
    {
        #[cfg(target_arch = "riscv64")]
        if available()
        {
            crate::toolbox::processes::termination::progress();
        }
    }
}
// The notification drops after the actual Arc, never before a final reader
// releases the stock it was preventing another capture from claiming.
pub(super) struct Archived
{
    data:      Arc<Data>,
    _released: ReleasedStock
}
impl Archived
{
    pub(super) fn data(&self) -> &Data
    {
        &self.data
    }
}
pub(super) fn archived(origin: Origin) -> Option<Archived>
{
    InvocationGate::run(|| {
        let pool = POOL.lock();
        let data = pool.archive.as_ref()?;
        (data.origin == Some(origin)).then(|| {
                                         Archived { data:      data.clone(),
                                                    _released: ReleasedStock }
                                     })
    })
}
#[derive(Clone)]
pub(super) struct Protected
{
    data:      Arc<Data>,
    _released: ReleasedStock
}
impl Protected
{
    fn claim() -> Result<Self, Status>
    {
        let claimed = InvocationGate::run(|| {
            let mut pool = POOL.lock();
            if !pool.initialized
            {
                return Err(NOT_SUPPORTED);
            }
            if pool.archive.as_ref().is_none_or(|data| Arc::strong_count(data) != 1)
            {
                return Err(SERVICE_BUSY);
            }
            pool.archive
                .take()
                .map(|data| {
                    Self { data,
                           _released: ReleasedStock }
                })
                .ok_or(SERVICE_BUSY)
        })?;
        #[cfg(target_arch = "riscv64")]
        crate::toolbox::processes::termination::progress();
        Ok(claimed)
    }

    fn restart(&mut self) -> Result<(), Status>
    {
        InvocationGate::run(|| {
            let mut pool = POOL.lock();
            match pool.archive.as_ref()
            {
                Some(data) if Arc::ptr_eq(data, &self.data) && Arc::strong_count(data) == 2 =>
                {
                    drop(pool.archive.take());
                }
                Some(_) => return Err(SERVICE_BUSY),
                None =>
                {}
            }
            if Arc::strong_count(&self.data) != 1
            {
                return Err(SERVICE_BUSY);
            }
            Ok(())
        })
    }

    pub(super) fn archive(&self)
    {
        InvocationGate::run(|| {
            let mut pool = POOL.lock();
            if pool.archive.is_none()
            {
                pool.archive = Some(self.data.clone());
            }
        });
    }
}
impl Drop for Protected
{
    fn drop(&mut self)
    {
        self.archive();
    }
}
pub(super) enum Payload
{
    Local(Data),
    Protected(Protected)
}
impl Default for Payload
{
    fn default() -> Self
    {
        Self::Local(Data::empty())
    }
}
impl Payload
{
    pub(super) fn protected(&self) -> bool
    {
        matches!(self, Self::Protected(_))
    }

    pub(super) fn origin(&self) -> Option<Origin>
    {
        self.data().origin
    }

    pub(super) fn from_archive(origin: Origin) -> Option<Self>
    {
        archived(origin).map(|data| {
                            Self::Protected(Protected { data:      data.data.clone(),
                                                        _released: ReleasedStock })
                        })
    }

    pub(super) fn data(&self) -> &Data
    {
        match self
        {
            Self::Local(data) => data,
            Self::Protected(data) => &data.data
        }
    }

    pub(super) fn data_mut(&mut self) -> Result<&mut Data, Status>
    {
        match self
        {
            Self::Local(data) => Ok(data),
            Self::Protected(data) => Arc::get_mut(&mut data.data).ok_or(SERVICE_BUSY)
        }
    }

    pub(super) fn protected_begin(&mut self) -> Result<(), Status>
    {
        match self
        {
            Self::Protected(data) => data.restart()?,
            Self::Local(_) => *self = Self::Protected(Protected::claim()?)
        }
        Ok(())
    }

    pub(super) fn archive(&self)
    {
        if let Self::Protected(data) = self
        {
            data.archive();
        }
    }

    pub(super) fn snapshot(&self) -> Result<Self, Status>
    {
        match self
        {
            Self::Protected(data) =>
            {
                data.archive();
                Ok(Self::Protected(data.clone()))
            }
            Self::Local(data) =>
            {
                let mut copy = Data::empty();
                copy.report.try_reserve_exact(data.report.len()).map_err(|_| NO_MEMORY)?;
                copy.starts.try_reserve_exact(data.starts.len()).map_err(|_| NO_MEMORY)?;
                copy.report.push_str(&data.report);
                copy.starts.extend_from_slice(&data.starts);
                copy.origin = data.origin;
                Ok(Self::Local(copy))
            }
        }
    }
}
