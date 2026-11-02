//! Full report storage. The native recovery slot is boot-admitted under System
//! authority; immutable snapshots share it without retaining the dying account.
#![forbid(unsafe_code)]
use alloc::{string::String, sync::Arc, vec::Vec};
use core::fmt;

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
    archive:     Option<Arc<Data>>,
    paint:       Option<String>
}
static POOL: Spinlock<Pool> = Spinlock::new(Pool { initialized: false,
                                                   archive:     None,
                                                   paint:       None });
pub(super) fn prepare(paint_bytes: usize) -> Result<(), Status>
{
    if InvocationGate::run(|| POOL.lock().initialized)
    {
        return Err(BAD_ABI);
    }
    let mut data = Data::empty();
    data.report.try_reserve_exact(MAX_REPORT_BYTES).map_err(|_| NO_MEMORY)?;
    data.starts.try_reserve_exact(MAX_REPORT_BYTES + 1).map_err(|_| NO_MEMORY)?;
    let data = Arc::try_new(data).map_err(|_| NO_MEMORY)?;
    let mut paint = String::new();
    paint.try_reserve_exact(paint_bytes).map_err(|_| NO_MEMORY)?;
    InvocationGate::run(|| {
        let mut pool = POOL.lock();
        if pool.initialized
        {
            return Err(BAD_ABI);
        }
        pool.archive = Some(data);
        pool.paint = Some(paint);
        pool.initialized = true;
        Ok(())
    })
}

#[derive(Clone, Copy, Debug)]
pub(in crate::toolbox) enum PaintError
{
    Unprepared,
    Busy,
    Capacity
    {
        required: usize,
        capacity: usize
    },
    Format
}
impl PaintError
{
    pub(in crate::toolbox) fn status(self) -> Status
    {
        match self
        {
            Self::Unprepared => NOT_SUPPORTED,
            Self::Busy => SERVICE_BUSY,
            Self::Capacity { .. } => BAD_ABI,
            Self::Format => mac_abi::BAD_ARGUMENT
        }
    }
}
impl fmt::Display for PaintError
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Unprepared => out.write_str("report paint scratch was not boot-admitted"),
            Self::Busy => out.write_str("report paint scratch is still borrowed"),
            Self::Capacity { required,
                             capacity } =>
            {
                write!(out,
                       "report paint scratch needs {required} bytes but admitted capacity is \
                        {capacity}")
            }
            Self::Format => out.write_str("report paint formatter refused the original value")
        }
    }
}

/// One affine staging buffer. Retained font_draw copies its borrowed bytes;
/// no read pin or account/Activity is retained when this loan returns.
#[must_use = "return the same report paint scratch on every paint exit"]
pub(in crate::toolbox) struct PaintScratch
{
    text: String
}
impl PaintScratch
{
    pub(in crate::toolbox) fn take() -> Result<Self, PaintError>
    {
        InvocationGate::run(|| {
            let mut pool = POOL.lock();
            if !pool.initialized
            {
                return Err(PaintError::Unprepared);
            }
            Ok(Self { text: pool.paint.take().ok_or(PaintError::Busy)? })
        })
    }

    pub(in crate::toolbox) fn clear(&mut self)
    {
        self.text.clear();
    }

    pub(in crate::toolbox) fn text(&self) -> &str
    {
        &self.text
    }

    pub(in crate::toolbox) fn admits(&self, required: usize) -> Result<(), PaintError>
    {
        let capacity = self.text.capacity();
        if required > capacity
        {
            return Err(PaintError::Capacity { required,
                                              capacity });
        }
        Ok(())
    }

    pub(in crate::toolbox) fn append(&mut self, value: &str) -> Result<(), PaintError>
    {
        let required = self.text
                           .len()
                           .checked_add(value.len())
                           .ok_or(PaintError::Capacity { required: usize::MAX,
                                                         capacity: self.text.capacity() })?;
        self.admits(required)?;
        self.text.push_str(value);
        Ok(())
    }

    pub(in crate::toolbox) fn write(&mut self, arguments: fmt::Arguments<'_>)
                                    -> Result<(), PaintError>
    {
        struct Writer<'a>
        {
            scratch: &'a mut PaintScratch,
            error:   Option<PaintError>
        }
        impl fmt::Write for Writer<'_>
        {
            fn write_str(&mut self, value: &str) -> fmt::Result
            {
                if self.error.is_some()
                {
                    return Err(fmt::Error);
                }
                self.scratch.append(value).map_err(|error| {
                                              self.error = Some(error);
                                              fmt::Error
                                          })
            }
        }
        let mut writer = Writer { scratch: self,
                                  error:   None };
        fmt::write(&mut writer, arguments).map_err(|_| writer.error.unwrap_or(PaintError::Format))
    }
}
impl Drop for PaintScratch
{
    fn drop(&mut self)
    {
        let mut text = core::mem::take(&mut self.text);
        text.clear();
        // This non-Clone loan is the sole writer to its absent stock slot.
        InvocationGate::run(|| POOL.lock().paint = Some(text));
    }
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
