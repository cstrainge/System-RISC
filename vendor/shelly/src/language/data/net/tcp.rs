
use std::{
    any::Any,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use tokio::{
    net::{TcpListener, TcpStream},
    sync::Notify,
};

use crate::{
    language::data::{
        io::handles::{Control, Reader, Writer},
        native_value::NativeData,
    },
    runtime::tasks::NativeFault,
};

#[derive(Clone)]
pub struct Connection
{
    pub reader: Reader,
    pub writer: Writer,
    pub peer: SocketAddr,
    pub local: SocketAddr,
}

impl Connection
{
    pub fn new(stream: TcpStream) -> Result<Self, NativeFault>
    {
        let peer = stream
            .peer_addr()
            .map_err(|error| NativeFault::io("OpenError", error))?;
        let local = stream
            .local_addr()
            .map_err(|error| NativeFault::io("OpenError", error))?;
        stream
            .set_nodelay(true)
            .map_err(|error| NativeFault::io("OpenError", error))?;
        let (reader, writer) = stream.into_split();
        let control = Control::new();
        Ok(Self {
            reader: Reader::new(reader, control.clone()),
            writer: Writer::new(writer, control),
            peer,
            local,
        })
    }
    pub fn close(&self)
    {
        self.reader.close_underlying();
        self.writer.close();
    }
}

pub struct ListenerState
{
    listener: Mutex<Option<Arc<TcpListener>>>,
    busy: AtomicBool,
    closed: AtomicBool,
    wake: Notify,
    pub address: SocketAddr,
}

#[derive(Clone)]
pub struct Listener(pub Arc<ListenerState>);

impl Listener
{
    pub fn new(listener: TcpListener) -> Result<Self, NativeFault>
    {
        let address = listener
            .local_addr()
            .map_err(|error| NativeFault::io("OpenError", error))?;
        Ok(Self(Arc::new(ListenerState {
            listener: Mutex::new(Some(Arc::new(listener))),
            busy: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            wake: Notify::new(),
            address,
        })))
    }
    pub fn close(&self)
    {
        self.0.closed.store(true, Ordering::Release);
        self.0.wake.notify_waiters();
        self.0.listener.lock().unwrap().take();
    }
    pub async fn accept(&self) -> Result<Connection, NativeFault>
    {
        let cancelled = self.0.wake.notified();
        tokio::pin!(cancelled);
        cancelled.as_mut().enable();
        if self.0.closed.load(Ordering::Acquire)
        {
            return Err(NativeFault::new("ClosedError", "Listener is closed"));
        }
        if self.0.busy.swap(true, Ordering::AcqRel)
        {
            return Err(NativeFault::new(
                "LeaseError",
                "Listener already has an accepting task",
            ));
        }
        struct Busy<'a>(&'a AtomicBool);
        impl Drop for Busy<'_>
        {
            fn drop(&mut self)
            {
                self.0.store(false, Ordering::Release);
            }
        }
        let _busy = Busy(&self.0.busy);
        let listener = self
            .0
            .listener
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| NativeFault::new("ClosedError", "Listener is closed"))?;
        let (stream, _) = tokio::select! {
            accepted = listener.accept() => accepted.map_err(|error| NativeFault::io("OpenError", error))?,
            _ = &mut cancelled => return Err(NativeFault::new("CancelledError", "Listener closed during accept")),
        };
        Connection::new(stream)
    }
}

impl NativeData for Connection
{
    fn type_name(&self) -> &'static str
    {
        "__tcp_Connection"
    }
    fn display(&self) -> String
    {
        format!("<TCP {}>", self.peer)
    }
    fn as_any(&self) -> &dyn Any
    {
        self
    }
    fn key(&self) -> Option<Vec<u8>>
    {
        Some(
            (Arc::as_ptr(&self.reader.0.control) as usize)
                .to_ne_bytes()
                .to_vec(),
        )
    }
}
impl NativeData for Listener
{
    fn type_name(&self) -> &'static str
    {
        "__tcp_Listener"
    }
    fn display(&self) -> String
    {
        format!("<TCP listener {}>", self.0.address)
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
