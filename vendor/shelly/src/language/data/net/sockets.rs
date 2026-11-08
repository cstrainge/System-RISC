
use std::{
    any::Any,
    future::Future,
    io,
    net::SocketAddr,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
};

use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpStream, UdpSocket},
};

use tokio_rustls::{
    TlsConnector,
    rustls::{
        ClientConfig, RootCertStore,
        pki_types::{CertificateDer, ServerName},
    },
};

use crate::{
    language::data::{
        io::{
            Bytes,
            handles::{Control, Reader, Writer},
        },
        native_value::NativeData,
        net::tcp::Connection as TcpConnection,
    },
    runtime::tasks::NativeFault,
};

struct DatagramState
{
    socket: Mutex<Option<Arc<UdpSocket>>>,
    control: Arc<Control>,
    busy: AtomicBool,
}
#[derive(Clone)]
pub struct DatagramSocket(Arc<DatagramState>);

impl DatagramSocket
{
    pub async fn bind(host: String, port: u16) -> Result<Self, NativeFault>
    {
        let socket = UdpSocket::bind((host, port))
            .await
            .map_err(|error| NativeFault::io("OpenError", error))?;
        Ok(Self(Arc::new(DatagramState {
            socket: Mutex::new(Some(Arc::new(socket))),
            control: Control::new(),
            busy: AtomicBool::new(false),
        })))
    }
    fn socket(&self) -> Result<Arc<UdpSocket>, NativeFault>
    {
        self.0
            .socket
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| NativeFault::new("ClosedError", "Datagram socket is closed"))
    }
    pub fn close(&self)
    {
        self.0.control.close();
        self.0.socket.lock().unwrap().take();
    }
    pub fn address(&self) -> Result<SocketAddr, NativeFault>
    {
        self.socket()?
            .local_addr()
            .map_err(|error| NativeFault::io("OpenError", error))
    }
    pub async fn send(&self, bytes: Bytes, host: String, port: u16) -> Result<usize, NativeFault>
    {
        let socket = self.socket()?;
        let cancelled = self.0.control.wake.notified();
        tokio::pin!(cancelled);
        cancelled.as_mut().enable();
        if self.0.control.closed.load(Ordering::Acquire)
        {
            return Err(NativeFault::new("ClosedError", "Datagram socket is closed"));
        }
        tokio::select! {
            count = socket.send_to(&bytes.0, (host, port)) =>
                count.map_err(|error| NativeFault::io("WriteError", error)),
            _ = cancelled => Err(NativeFault::new("CancelledError", "Datagram send cancelled")),
        }
    }
    pub async fn receive(&self, maximum: usize) -> Result<(Bytes, SocketAddr, bool), NativeFault>
    {
        if self.0.busy.swap(true, Ordering::AcqRel)
        {
            return Err(NativeFault::new(
                "LeaseError",
                "Datagram socket already has a receiver",
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
        let socket = self.socket()?;
        let cancelled = self.0.control.wake.notified();
        tokio::pin!(cancelled);
        cancelled.as_mut().enable();
        if self.0.control.closed.load(Ordering::Acquire)
        {
            return Err(NativeFault::new("ClosedError", "Datagram socket is closed"));
        }
        let mut buffer = vec![0; maximum + 1];
        let (count, peer) = tokio::select! {
            packet = socket.recv_from(&mut buffer) =>
                packet.map_err(|error| NativeFault::io("ReadError", error))?,
            _ = cancelled => return Err(NativeFault::new("CancelledError", "Datagram receive cancelled")),
        };
        buffer.truncate(count.min(maximum));
        Ok((Bytes(buffer.into()), peer, count > maximum))
    }
}

impl NativeData for DatagramSocket
{
    fn type_name(&self) -> &'static str
    {
        "__socket_DatagramSocket"
    }
    fn display(&self) -> String
    {
        "<UDP socket>".into()
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

#[derive(Clone)]
pub struct Connection
{
    pub reader: Reader,
    pub writer: Writer,
}
impl Connection
{
    pub fn new(stream: impl AsyncRead + AsyncWrite + Send + 'static) -> Self
    {
        let (reader, writer) = tokio::io::split(stream);
        let control = Control::new();
        Self {
            reader: Reader::new(reader, control.clone()),
            writer: Writer::new(writer, control),
        }
    }
    pub fn close(&self)
    {
        self.reader.close_underlying();
        self.writer.close_underlying();
    }
}
impl NativeData for Connection
{
    fn type_name(&self) -> &'static str
    {
        "__socket_Connection"
    }
    fn display(&self) -> String
    {
        "<Stream connection>".into()
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

// An upgrade owns both transport leases. Existing aliases can close it, but cannot read or
// write plaintext while TLS uses it. Dropping a failed handshake invalidates the transport.
struct Bridge
{
    reader: Reader,
    writer: Writer,
    input: Vec<u8>,
    read: Option<Pin<Box<dyn Future<Output = Result<Option<Bytes>, NativeFault>> + Send>>>,
    write: Option<Pin<Box<dyn Future<Output = Result<usize, NativeFault>> + Send>>>,
    flush: Option<Pin<Box<dyn Future<Output = Result<(), NativeFault>> + Send>>>,
    shutdown: Option<Pin<Box<dyn Future<Output = Result<(), NativeFault>> + Send>>>,
}
impl Drop for Bridge
{
    fn drop(&mut self)
    {
        self.reader.close_underlying();
        self.writer.close_underlying();
    }
}
impl AsyncRead for Bridge
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>>
    {
        if !self.input.is_empty()
        {
            let count = self.input.len().min(buffer.remaining());
            buffer.put_slice(&self.input[..count]);
            self.input.drain(..count);
            return Poll::Ready(Ok(()));
        }
        if buffer.remaining() == 0
        {
            return Poll::Ready(Ok(()));
        }
        if self.read.is_none()
        {
            let reader = self.reader.clone();
            self.read = Some(Box::pin(async move { reader.read(65536).await }));
        }
        match self.read.as_mut().unwrap().as_mut().poll(context)
        {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) =>
            {
                self.read = None;
                match result
                {
                    Ok(None) => Poll::Ready(Ok(())),
                    Ok(Some(bytes)) =>
                    {
                        self.input = bytes.0.to_vec();
                        self.poll_read(context, buffer)
                    }
                    Err(error) => Poll::Ready(Err(io::Error::other(error.message))),
                }
            }
        }
    }
}
impl AsyncWrite for Bridge
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>>
    {
        // Accept at most one owned buffer. Reporting an earlier operation's byte count for a
        // different retry buffer would violate AsyncWrite after cancellation.
        if let Some(write) = &mut self.write
        {
            match write.as_mut().poll(context)
            {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) =>
                {
                    self.write = None;
                    return Poll::Ready(Err(io::Error::other(error.message)));
                }
                Poll::Ready(Ok(_)) => self.write = None,
            }
        }
        let writer = self.writer.clone();
        let count = bytes.len().min(65536);
        let bytes = Bytes(Arc::from(&bytes[..count]));
        self.write = Some(Box::pin(async move { writer.write(bytes).await }));
        Poll::Ready(Ok(count))
    }
    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>>
    {
        if let Some(write) = &mut self.write
        {
            match write.as_mut().poll(context)
            {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) =>
                {
                    self.write = None;
                    return Poll::Ready(Err(io::Error::other(error.message)));
                }
                Poll::Ready(Ok(_)) => self.write = None,
            }
        }
        if self.flush.is_none()
        {
            let writer = self.writer.clone();
            self.flush = Some(Box::pin(async move { writer.flush().await }));
        }
        match self.flush.as_mut().unwrap().as_mut().poll(context)
        {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) =>
            {
                self.flush = None;
                Poll::Ready(result.map_err(|error| io::Error::other(error.message)))
            }
        }
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>>
    {
        match self.as_mut().poll_flush(context)
        {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(())) =>
            {}
        }
        if self.shutdown.is_none()
        {
            let writer = self.writer.clone();
            self.shutdown = Some(Box::pin(async move { writer.shutdown().await }));
        }
        match self.shutdown.as_mut().unwrap().as_mut().poll(context)
        {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) =>
            {
                Poll::Ready(result.map_err(|error| io::Error::other(error.message)))
            }
        }
    }
}

pub async fn tls_wrap(
    connection: TcpConnection,
    hostname: String,
    certificate: Option<Bytes>,
) -> Result<Connection, NativeFault>
{
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(certificate) = certificate
    {
        roots
            .add(CertificateDer::from(certificate.0.to_vec()))
            .map_err(|error| NativeFault::new("OpenError", error.to_string()))?;
    }
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let hostname = ServerName::try_from(hostname)
        .map_err(|_| NativeFault::new("OpenError", "Invalid TLS server name"))?;
    let reader = connection.reader.leased()?;
    let writer = connection.writer.leased()?;
    let bridge = Bridge {
        reader,
        writer,
        input: Vec::new(),
        read: None,
        write: None,
        flush: None,
        shutdown: None,
    };
    let stream = TlsConnector::from(Arc::new(config))
        .connect(hostname, bridge)
        .await
        .map_err(|error| NativeFault::io("OpenError", error))?;
    Ok(Connection::new(stream))
}

pub async fn tls_connect(
    host: String,
    port: u16,
    certificate: Option<Bytes>,
) -> Result<Connection, NativeFault>
{
    let stream = TcpStream::connect((host.clone(), port))
        .await
        .map_err(|error| NativeFault::io("OpenError", error))?;
    tls_wrap(TcpConnection::new(stream)?, host, certificate).await
}

#[cfg(unix)]
pub mod unix
{
    use super::*;
    use tokio::net::UnixListener;

    struct State
    {
        listener: Mutex<Option<Arc<UnixListener>>>,
        control: Arc<Control>,
        busy: AtomicBool,
    }
    #[derive(Clone)]
    pub struct Listener(Arc<State>);
    impl Listener
    {
        pub fn bind(path: String) -> Result<Self, NativeFault>
        {
            let listener =
                UnixListener::bind(path).map_err(|error| NativeFault::io("OpenError", error))?;
            Ok(Self(Arc::new(State {
                listener: Mutex::new(Some(Arc::new(listener))),
                control: Control::new(),
                busy: AtomicBool::new(false),
            })))
        }
        pub fn close(&self)
        {
            self.0.control.close();
            self.0.listener.lock().unwrap().take();
        }
        pub async fn accept(&self) -> Result<Connection, NativeFault>
        {
            if self.0.busy.swap(true, Ordering::AcqRel)
            {
                return Err(NativeFault::new(
                    "LeaseError",
                    "Unix listener already has an accepting task",
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
                .ok_or_else(|| NativeFault::new("ClosedError", "Unix listener is closed"))?;
            let cancelled = self.0.control.wake.notified();
            tokio::pin!(cancelled);
            cancelled.as_mut().enable();
            if self.0.control.closed.load(Ordering::Acquire)
            {
                return Err(NativeFault::new("ClosedError", "Unix listener is closed"));
            }
            let (stream, _) = tokio::select! {
                accepted = listener.accept() => accepted.map_err(|error| NativeFault::io("OpenError", error))?,
                _ = cancelled => return Err(NativeFault::new("CancelledError", "Unix accept cancelled")),
            };
            Ok(Connection::new(stream))
        }
    }
    impl NativeData for Listener
    {
        fn type_name(&self) -> &'static str
        {
            "__socket_Listener"
        }
        fn display(&self) -> String
        {
            "<Unix listener>".into()
        }
        fn as_any(&self) -> &dyn Any
        {
            self
        }
    }
}
