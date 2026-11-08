
use std::{
    any::Any,
    io::Cursor,
    pin::Pin,
    sync::{
        Arc, Mutex as StorageMutex, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{Mutex, Notify},
};

use crate::{
    language::data::{io::Bytes, native_value::NativeData},
    runtime::tasks::NativeFault,
};

type Readable = Pin<Box<dyn AsyncRead + Send>>;
type Writable = Pin<Box<dyn AsyncWrite + Send>>;
static NEXT_LEASE: AtomicU64 = AtomicU64::new(1);
static READ_LEASES: AtomicUsize = AtomicUsize::new(0);

pub struct Control
{
    pub closed: Arc<AtomicBool>,
    pub wake: Notify,
    storage: StorageMutex<Vec<Weak<dyn Storage>>>,
}

trait Storage: Send + Sync
{
    fn release(&self);
}

impl Control
{
    pub fn new() -> Arc<Self>
    {
        Arc::new(Self {
            closed: Arc::new(AtomicBool::new(false)),
            wake: Notify::new(),
            storage: StorageMutex::new(Vec::new()),
        })
    }
    pub fn close(&self)
    {
        self.closed.store(true, Ordering::Release);
        self.wake.notify_waiters();
        for storage in self
            .storage
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
        {
            storage.release();
        }
    }
    fn register(&self, storage: Arc<dyn Storage>)
    {
        self.storage.lock().unwrap().push(Arc::downgrade(&storage));
    }
}

pub struct ReaderState
{
    pub control: Arc<Control>,
    pub source: Mutex<Option<Readable>>,
    busy: AtomicBool,
    eof: AtomicBool,
    consumer: AtomicU64,
}

pub struct ReadLease
{
    id: u64,
    state: Weak<ReaderState>,
    active: AtomicBool,
    wake: Notify,
}

impl ReadLease
{
    fn release(&self)
    {
        if self.active.swap(false, Ordering::AcqRel)
        {
            if let Some(state) = self.state.upgrade()
            {
                let _ = state.consumer.compare_exchange(
                    self.id,
                    0,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
            }
            self.wake.notify_waiters();
            READ_LEASES.fetch_sub(1, Ordering::AcqRel);
        }
    }
}
impl Drop for ReadLease
{
    fn drop(&mut self)
    {
        self.release();
    }
}

#[derive(Clone)]
pub struct Reader(pub Arc<ReaderState>, Option<Arc<ReadLease>>, bool);

pub struct WriterState
{
    pub control: Arc<Control>,
    pub sink: Mutex<Option<Writable>>,
    ended: AtomicBool,
    busy: AtomicBool,
    consumer: AtomicU64,
}

#[derive(Clone)]
pub struct Writer(pub Arc<WriterState>, Option<Arc<WriteLease>>, bool);

pub struct WriteLease
{
    id: u64,
    state: Weak<WriterState>,
    active: AtomicBool,
    wake: Notify,
}
impl WriteLease
{
    fn release(&self)
    {
        if self.active.swap(false, Ordering::AcqRel)
        {
            if let Some(state) = self.state.upgrade()
            {
                let _ = state.consumer.compare_exchange(
                    self.id,
                    0,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
            }
            self.wake.notify_waiters();
            READ_LEASES.fetch_sub(1, Ordering::AcqRel);
        }
    }
}
impl Drop for WriteLease
{
    fn drop(&mut self)
    {
        self.release();
    }
}

struct Busy<'a>(&'a AtomicBool);
impl Drop for Busy<'_>
{
    fn drop(&mut self)
    {
        self.0.store(false, Ordering::Release);
    }
}

impl Storage for ReaderState
{
    fn release(&self)
    {
        if let Ok(mut storage) = self.source.try_lock()
        {
            storage.take();
        }
    }
}
impl Storage for WriterState
{
    fn release(&self)
    {
        if let Ok(mut storage) = self.sink.try_lock()
        {
            storage.take();
        }
    }
}

struct ReleaseClosed<'a>(&'a Control, &'a dyn Storage);
impl Drop for ReleaseClosed<'_>
{
    fn drop(&mut self)
    {
        if self.0.closed.load(Ordering::Acquire)
        {
            self.1.release();
        }
    }
}

impl Reader
{
    pub fn new(source: impl AsyncRead + Send + 'static, control: Arc<Control>) -> Self
    {
        let state = Arc::new(ReaderState {
            control: control.clone(),
            source: Mutex::new(Some(Box::pin(source))),
            busy: AtomicBool::new(false),
            eof: AtomicBool::new(false),
            consumer: AtomicU64::new(0),
        });
        control.register(state.clone());
        Self(state, None, false)
    }
    pub fn memory(bytes: Bytes) -> Self
    {
        Self::new(Cursor::new(bytes.0), Control::new())
    }
    pub fn close(&self)
    {
        if let Some(lease) = &self.1
        {
            if self.2
            {
                lease.release();
            }
            return;
        }
        self.close_underlying();
    }
    pub fn close_underlying(&self)
    {
        self.0.control.close();
        if let Ok(mut source) = self.0.source.try_lock()
        {
            source.take();
        }
    }
    pub fn leased(&self) -> Result<Self, NativeFault>
    {
        if let Some(lease) = &self.1
        {
            if !lease.active.load(Ordering::Acquire)
            {
                return Err(NativeFault::new("LeaseError", "Reader lease has expired"));
            }
            return Ok(Self(self.0.clone(), self.1.clone(), false));
        }
        if READ_LEASES.fetch_add(1, Ordering::AcqRel) >= 256
        {
            READ_LEASES.fetch_sub(1, Ordering::AcqRel);
            return Err(NativeFault::new(
                "LimitError",
                "Too many active reader leases",
            ));
        }
        let id = NEXT_LEASE.fetch_add(1, Ordering::Relaxed);
        if self
            .0
            .consumer
            .compare_exchange(0, id, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            READ_LEASES.fetch_sub(1, Ordering::AcqRel);
            return Err(NativeFault::new(
                "LeaseError",
                "Reader already has a consumer",
            ));
        }
        Ok(Self(
            self.0.clone(),
            Some(Arc::new(ReadLease {
                id,
                state: Arc::downgrade(&self.0),
                active: AtomicBool::new(true),
                wake: Notify::new(),
            })),
            true,
        ))
    }
    pub fn borrowed(&self) -> Self
    {
        Self(self.0.clone(), self.1.clone(), false)
    }
    pub async fn read(&self, maximum: usize) -> Result<Option<Bytes>, NativeFault>
    {
        let id = self
            .1
            .as_ref()
            .filter(|lease| lease.active.load(Ordering::Acquire))
            .map_or(0, |lease| lease.id);
        if self.0.consumer.load(Ordering::Acquire) != id || self.1.is_some() && id == 0
        {
            return Err(NativeFault::new(
                "LeaseError",
                "Reader is reserved by another consumer",
            ));
        }
        if self.0.control.closed.load(Ordering::Acquire)
        {
            return Err(NativeFault::new("ClosedError", "Reader is closed"));
        }
        if self.0.eof.load(Ordering::Acquire)
        {
            return Ok(None);
        }
        if self.0.busy.swap(true, Ordering::AcqRel)
        {
            return Err(NativeFault::new(
                "LeaseError",
                "Reader already has a consumer",
            ));
        }
        let _busy = Busy(&self.0.busy);
        let _release = ReleaseClosed(&self.0.control, self.0.as_ref());
        let cancelled = self.0.control.wake.notified();
        tokio::pin!(cancelled);
        cancelled.as_mut().enable();
        if self.0.control.closed.load(Ordering::Acquire)
        {
            return Err(NativeFault::new("ClosedError", "Reader is closed"));
        }
        let lease_cancelled = async {
            if let Some(lease) = &self.1
            {
                let notified = lease.wake.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if lease.active.load(Ordering::Acquire)
                {
                    notified.await;
                }
            }
            else
            {
                std::future::pending::<()>().await;
            }
        };
        let mut storage = self.0.source.lock().await;
        let source = storage
            .as_mut()
            .ok_or_else(|| NativeFault::new("ClosedError", "Reader is closed"))?;
        let mut buffer = vec![0; maximum];
        let count = tokio::select! {
            count = source.read(&mut buffer) => count.map_err(|error|
                NativeFault::io("ReadError", error))?,
            _ = &mut cancelled => return Err(NativeFault::new("CancelledError", "Reader closed during read")),
            _ = lease_cancelled => return Err(NativeFault::new("CancelledError", "Reader lease released during read")),
        };
        if count == 0
        {
            self.0.eof.store(true, Ordering::Release);
            // Taking storage here releases file and transport handles at EOF.
            storage.take();
            return Ok(None);
        }
        buffer.truncate(count);
        Ok(Some(Bytes(buffer.into())))
    }
}

impl Writer
{
    pub fn new(sink: impl AsyncWrite + Send + 'static, control: Arc<Control>) -> Self
    {
        let state = Arc::new(WriterState {
            control: control.clone(),
            sink: Mutex::new(Some(Box::pin(sink))),
            ended: AtomicBool::new(false),
            busy: AtomicBool::new(false),
            consumer: AtomicU64::new(0),
        });
        control.register(state.clone());
        Self(state, None, false)
    }
    pub fn close(&self)
    {
        if let Some(lease) = &self.1
        {
            if self.2
            {
                lease.release();
            }
            return;
        }
        self.close_underlying();
    }
    pub fn close_underlying(&self)
    {
        self.0.control.close();
        if let Ok(mut sink) = self.0.sink.try_lock()
        {
            sink.take();
        }
    }
    pub fn leased(&self) -> Result<Self, NativeFault>
    {
        if let Some(lease) = &self.1
        {
            if !lease.active.load(Ordering::Acquire)
            {
                return Err(NativeFault::new("LeaseError", "Writer lease has expired"));
            }
            return Ok(Self(self.0.clone(), self.1.clone(), false));
        }
        if READ_LEASES.fetch_add(1, Ordering::AcqRel) >= 256
        {
            READ_LEASES.fetch_sub(1, Ordering::AcqRel);
            return Err(NativeFault::new("LimitError", "Too many active I/O leases"));
        }
        let id = NEXT_LEASE.fetch_add(1, Ordering::Relaxed);
        if self
            .0
            .consumer
            .compare_exchange(0, id, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            READ_LEASES.fetch_sub(1, Ordering::AcqRel);
            return Err(NativeFault::new(
                "LeaseError",
                "Writer already has a producer",
            ));
        }
        Ok(Self(
            self.0.clone(),
            Some(Arc::new(WriteLease {
                id,
                state: Arc::downgrade(&self.0),
                active: AtomicBool::new(true),
                wake: Notify::new(),
            })),
            true,
        ))
    }
    fn check_lease(&self) -> Result<(), NativeFault>
    {
        let id = self
            .1
            .as_ref()
            .filter(|lease| lease.active.load(Ordering::Acquire))
            .map_or(0, |lease| lease.id);
        if self.0.consumer.load(Ordering::Acquire) != id || self.1.is_some() && id == 0
        {
            return Err(NativeFault::new(
                "LeaseError",
                "Writer is reserved by another producer",
            ));
        }
        Ok(())
    }
    pub async fn shutdown(&self) -> Result<(), NativeFault>
    {
        if self.0.ended.load(Ordering::Acquire)
        {
            return Ok(());
        }
        self.check_lease()?;
        if self.0.busy.swap(true, Ordering::AcqRel)
        {
            return Err(NativeFault::new(
                "LeaseError",
                "Writer already has a producer",
            ));
        }
        let _busy = Busy(&self.0.busy);
        let mut storage = self.0.sink.lock().await;
        if let Some(sink) = storage.as_mut()
        {
            tokio::select! {
                result = sink.shutdown() => result.map_err(|error| NativeFault::io("WriteError", error))?,
                _ = self.0.control.wake.notified() =>
                    return Err(NativeFault::new("CancelledError", "Write shutdown cancelled")),
            }
        }
        self.0.ended.store(true, Ordering::Release);
        storage.take();
        Ok(())
    }
    pub async fn write(&self, bytes: Bytes) -> Result<usize, NativeFault>
    {
        self.check_lease()?;
        if self.0.ended.load(Ordering::Acquire)
        {
            return Err(NativeFault::new(
                "ClosedError",
                "Write side has been shut down",
            ));
        }
        if self.0.control.closed.load(Ordering::Acquire)
        {
            return Err(NativeFault::new("ClosedError", "Writer is closed"));
        }
        if self.0.busy.swap(true, Ordering::AcqRel)
        {
            return Err(NativeFault::new(
                "LeaseError",
                "Writer already has a producer",
            ));
        }
        let _busy = Busy(&self.0.busy);
        let _release = ReleaseClosed(&self.0.control, self.0.as_ref());
        let cancelled = self.0.control.wake.notified();
        tokio::pin!(cancelled);
        cancelled.as_mut().enable();
        if self.0.control.closed.load(Ordering::Acquire)
        {
            return Err(NativeFault::new("ClosedError", "Writer is closed"));
        }
        let lease_cancelled = async {
            if let Some(lease) = &self.1
            {
                let notified = lease.wake.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if lease.active.load(Ordering::Acquire)
                {
                    notified.await;
                }
            }
            else
            {
                std::future::pending::<()>().await;
            }
        };
        let mut sink = self.0.sink.lock().await;
        let sink = sink
            .as_mut()
            .ok_or_else(|| NativeFault::new("ClosedError", "Writer is closed"))?;
        tokio::select! {
            result = async { sink.write_all(&bytes.0).await?; sink.flush().await } => result.map_err(|error|
                NativeFault::io("WriteError", error))?,
            _ = &mut cancelled => return Err(NativeFault::new("CancelledError", "Writer closed during write")),
            _ = lease_cancelled => return Err(NativeFault::new("CancelledError", "Writer lease released during write")),
        };
        Ok(bytes.0.len())
    }
    pub async fn flush(&self) -> Result<(), NativeFault>
    {
        self.check_lease()?;
        if self.0.ended.load(Ordering::Acquire)
        {
            return Err(NativeFault::new(
                "ClosedError",
                "Write side has been shut down",
            ));
        }
        if self.0.control.closed.load(Ordering::Acquire)
        {
            return Err(NativeFault::new("ClosedError", "Writer is closed"));
        }
        if self.0.busy.swap(true, Ordering::AcqRel)
        {
            return Err(NativeFault::new(
                "LeaseError",
                "Writer already has a producer",
            ));
        }
        let _busy = Busy(&self.0.busy);
        let _release = ReleaseClosed(&self.0.control, self.0.as_ref());
        let lease_cancelled = async {
            if let Some(lease) = &self.1
            {
                let notified = lease.wake.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if lease.active.load(Ordering::Acquire)
                {
                    notified.await;
                }
            }
            else
            {
                std::future::pending::<()>().await;
            }
        };
        let mut sink = self.0.sink.lock().await;
        let sink = sink
            .as_mut()
            .ok_or_else(|| NativeFault::new("ClosedError", "Writer is closed"))?;
        tokio::select! {
            result = sink.flush() => result.map_err(|error| NativeFault::io("WriteError", error)),
            _ = self.0.control.wake.notified() => Err(NativeFault::new("CancelledError", "Writer closed during flush")),
            _ = lease_cancelled => Err(NativeFault::new("CancelledError", "Writer lease released during flush")),
        }
    }
}

impl NativeData for Reader
{
    fn type_name(&self) -> &'static str
    {
        "__io_Reader"
    }
    fn display(&self) -> String
    {
        "<Reader>".into()
    }
    fn as_any(&self) -> &dyn Any
    {
        self
    }
    fn key(&self) -> Option<Vec<u8>>
    {
        Some((Arc::as_ptr(&self.0) as usize).to_ne_bytes().to_vec())
    }
}

impl NativeData for Writer
{
    fn type_name(&self) -> &'static str
    {
        "__io_Writer"
    }
    fn display(&self) -> String
    {
        "<Writer>".into()
    }
    fn as_any(&self) -> &dyn Any
    {
        self
    }
    fn key(&self) -> Option<Vec<u8>>
    {
        Some((Arc::as_ptr(&self.0) as usize).to_ne_bytes().to_vec())
    }
}
