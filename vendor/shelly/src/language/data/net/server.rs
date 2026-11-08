
use std::{
    any::Any,
    convert::Infallible,
    io,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use bytes::Bytes as WireBytes;

use futures_util::{StreamExt, stream::try_unfold};

use http_body_util::{BodyExt, Full, StreamBody, combinators::UnsyncBoxBody};

use hyper::{
    Request, Response,
    body::{Frame, Incoming as Body},
    header::{HeaderName, HeaderValue},
    server::conn::http1::Builder,
    service::service_fn,
};

use hyper_util::rt::{TokioIo, TokioTimer};

use tokio::{
    net::TcpListener,
    sync::{Mutex as AsyncMutex, Notify, Semaphore, mpsc, oneshot},
    task::AbortHandle,
};

use tokio_util::io::StreamReader;

use crate::{
    language::data::{
        io::{
            Bytes,
            adapters::ByteSource,
            handles::{Control, Reader},
        },
        native_value::NativeData,
    },
    runtime::tasks::NativeFault,
};

struct CloseOnDrop(Arc<Control>);
impl Drop for CloseOnDrop
{
    fn drop(&mut self)
    {
        self.0.close();
    }
}

type OutgoingBody = UnsyncBoxBody<WireBytes, io::Error>;
pub enum Outgoing
{
    Empty,
    Bytes(Bytes),
    Reader(Reader),
    Source(ByteSource),
}
pub struct Reply
{
    pub status: u16,
    pub headers: Vec<(String, Vec<String>)>,
    pub body: Outgoing,
}

struct IncomingState
{
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, Vec<String>)>,
    pub body: Mutex<Option<Reader>>,
    pub control: Arc<Control>,
    pub cancel: Arc<AtomicBool>,
    pub reply: Mutex<Option<oneshot::Sender<Response<OutgoingBody>>>>,
    pub completed: AsyncMutex<Option<oneshot::Receiver<Result<(), NativeFault>>>>,
    pub deadline: std::time::Instant,
}
#[derive(Clone)]
pub struct Incoming(Arc<IncomingState>);
impl Incoming
{
    pub fn method(&self) -> &str
    {
        &self.0.method
    }
    pub fn path(&self) -> &str
    {
        &self.0.path
    }
    pub fn headers(&self) -> &[(String, Vec<String>)]
    {
        &self.0.headers
    }
    pub fn cancelled(&self) -> Arc<AtomicBool>
    {
        self.0.cancel.clone()
    }
    pub fn deadline(&self) -> std::time::Instant
    {
        self.0.deadline
    }
    pub fn active(&self) -> bool
    {
        self.0.reply.lock().unwrap().is_some()
    }
    pub fn take_body(&self) -> Result<Reader, NativeFault>
    {
        self.0
            .body
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| NativeFault::new("ClosedError", "Request body was already taken"))
    }
    pub fn close(&self)
    {
        self.0.cancel.store(true, Ordering::Release);
        self.0.control.close();
        if let Some(reader) = self.0.body.lock().unwrap().take()
        {
            reader.close_underlying();
        }
        if let Some(reply) = self.0.reply.lock().unwrap().take()
        {
            let _ = reply.send(plain(500, "Internal server error"));
        }
    }
    pub async fn finish_close(&self) -> Result<(), NativeFault>
    {
        self.close();
        if let Some(completed) = self.0.completed.lock().await.take()
        {
            tokio::time::timeout(Duration::from_millis(500), completed)
                .await
                .map_err(|_| NativeFault::new("TimeoutError", "Fallback response timed out"))?
                .map_err(|_| NativeFault::new("CancelledError", "HTTP peer disconnected"))??;
        }
        Ok(())
    }
    pub async fn respond(&self, mut reply: Reply) -> Result<(), NativeFault>
    {
        if matches!(reply.status, 204 | 205 | 304) || self.method() == "HEAD"
        {
            match &reply.body
            {
                Outgoing::Reader(reader) => reader.close(),
                Outgoing::Source(source) => source.close(),
                _ =>
                {}
            }
            reply.body = Outgoing::Empty;
        }
        let reply = encode_reply(reply)?;
        self.0
            .reply
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| NativeFault::new("ClosedError", "Request already has a response"))?
            .send(reply)
            .map_err(|_| NativeFault::new("CancelledError", "HTTP peer disconnected"))?;
        let result = self
            .0
            .completed
            .lock()
            .await
            .take()
            .ok_or_else(|| {
                NativeFault::new("LeaseError", "Response already has a finishing consumer")
            })?
            .await
            .map_err(|_| NativeFault::new("CancelledError", "HTTP connection closed"))?;
        self.0.control.close();
        result
    }
}
impl NativeData for Incoming
{
    fn type_name(&self) -> &'static str
    {
        "__server_Incoming"
    }
    fn display(&self) -> String
    {
        "<HTTP request>".into()
    }
    fn as_any(&self) -> &dyn Any
    {
        self
    }
    fn operation(&self) -> bool
    {
        self.active()
    }
    fn release(&self)
    {
        self.close();
    }
}

struct State
{
    address: SocketAddr,
    receiver: AsyncMutex<mpsc::Receiver<Incoming>>,
    accept: AbortHandle,
    connections: Arc<Mutex<Vec<AbortHandle>>>,
    closed: AtomicBool,
    wake: Notify,
}
#[derive(Clone)]
pub struct Server(Arc<State>);
impl Server
{
    pub async fn bind(host: String, port: u16) -> Result<Self, NativeFault>
    {
        let listener = TcpListener::bind((host, port))
            .await
            .map_err(|error| NativeFault::io("OpenError", error))?;
        let address = listener
            .local_addr()
            .map_err(|error| NativeFault::io("OpenError", error))?;
        let (sender, receiver) = mpsc::channel(16);
        let connections = Arc::new(Mutex::new(Vec::<AbortHandle>::new()));
        let workers = connections.clone();
        let permits = Arc::new(Semaphore::new(32));
        let task = tokio::spawn(async move {
            loop
            {
                let Ok(permit) = permits.clone().acquire_owned().await
                else
                {
                    break;
                };
                let Ok((stream, _)) = listener.accept().await
                else
                {
                    break;
                };
                let sender = sender.clone();
                let completed: Arc<Mutex<Option<oneshot::Sender<Result<(), NativeFault>>>>> =
                    Arc::new(Mutex::new(None));
                let finished = completed.clone();
                let control = Control::new();
                let cancelled = control.clone();
                let task = tokio::spawn(async move {
                    let _permit = permit;
                    let _closed = CloseOnDrop(cancelled.clone());
                    let service = service_fn(move |request: Request<Body>| {
                        let sender = sender.clone();
                        let completed = completed.clone();
                        let control = control.clone();
                        async move {
                            let target = request.uri().to_string();
                            if request.method() == hyper::Method::CONNECT
                                || !target.starts_with('/')
                            {
                                return Ok::<_, Infallible>(plain(
                                    400,
                                    "Unsupported request target",
                                ));
                            }
                            let headers = request
                                .headers()
                                .keys()
                                .map(|name| {
                                    let values = request
                                        .headers()
                                        .get_all(name)
                                        .iter()
                                        .map(|value| {
                                            std::str::from_utf8(value.as_bytes())
                                                .map(str::to_string)
                                        })
                                        .collect::<Result<Vec<_>, _>>()?;
                                    Ok::<_, std::str::Utf8Error>((
                                        name.as_str().to_string(),
                                        values,
                                    ))
                                })
                                .collect::<Result<Vec<_>, _>>();
                            let Ok(headers) = headers
                            else
                            {
                                return Ok(plain(400, "Unsupported header encoding"));
                            };
                            let method = request.method().as_str().to_string();
                            let stream = request
                                .into_body()
                                .into_data_stream()
                                .map(|result| result.map_err(io::Error::other));
                            let body_control = Control::new();
                            let body = Reader::new(StreamReader::new(stream), body_control.clone());
                            let (reply, response) = oneshot::channel();
                            let (done, done_receiver) = oneshot::channel();
                            *completed.lock().unwrap() = Some(done);
                            let incoming = Incoming(Arc::new(IncomingState {
                                method,
                                path: target,
                                headers,
                                body: Mutex::new(Some(body)),
                                control: body_control,
                                cancel: control.closed.clone(),
                                reply: Mutex::new(Some(reply)),
                                completed: AsyncMutex::new(Some(done_receiver)),
                                deadline: std::time::Instant::now() + Duration::from_secs(30),
                            }));
                            if sender.send(incoming).await.is_err()
                            {
                                return Ok(plain(503, "Server is closed"));
                            }
                            Ok(response
                                .await
                                .unwrap_or_else(|_| plain(500, "Internal server error")))
                        }
                    });
                    let mut builder = Builder::new();
                    builder
                        .keep_alive(false)
                        .max_buf_size(65536)
                        .timer(TokioTimer::new())
                        .header_read_timeout(Duration::from_secs(30));
                    let result = tokio::time::timeout(
                        Duration::from_secs(30),
                        builder.serve_connection(TokioIo::new(stream), service),
                    )
                    .await;
                    cancelled.close();
                    if let Some(done) = finished.lock().unwrap().take()
                    {
                        let result = match result
                        {
                            Ok(Ok(())) => Ok(()),
                            Ok(Err(error)) => Err(NativeFault::error("WriteError", &error)),
                            Err(_) => Err(NativeFault::new(
                                "TimeoutError",
                                "Server exchange timed out",
                            )),
                        };
                        let _ = done.send(result);
                    }
                });
                let mut workers = workers.lock().unwrap();
                workers.retain(|worker| !worker.is_finished());
                workers.push(task.abort_handle());
            }
        });
        Ok(Self(Arc::new(State {
            address,
            receiver: AsyncMutex::new(receiver),
            accept: task.abort_handle(),
            connections,
            closed: AtomicBool::new(false),
            wake: Notify::new(),
        })))
    }
    pub fn address(&self) -> SocketAddr
    {
        self.0.address
    }
    pub fn close(&self)
    {
        self.0.closed.store(true, Ordering::Release);
        self.0.wake.notify_waiters();
        self.0.accept.abort();
        for task in self.0.connections.lock().unwrap().drain(..)
        {
            task.abort();
        }
    }
    pub async fn accept(&self) -> Result<Incoming, NativeFault>
    {
        let cancelled = self.0.wake.notified();
        tokio::pin!(cancelled);
        cancelled.as_mut().enable();
        if self.0.closed.load(Ordering::Acquire)
        {
            return Err(NativeFault::new("ClosedError", "Server is closed"));
        }
        let mut receiver =
            self.0.receiver.try_lock().map_err(|_| {
                NativeFault::new("LeaseError", "Server already has a request consumer")
            })?;
        tokio::select! {
            request = receiver.recv() => request.ok_or_else(|| NativeFault::new("ClosedError", "Server stopped accepting")),
            _ = cancelled => Err(NativeFault::new("CancelledError", "Server accept cancelled")),
        }
    }
}
impl Drop for State
{
    fn drop(&mut self)
    {
        self.accept.abort();
        for task in self.connections.lock().unwrap().iter()
        {
            task.abort();
        }
    }
}
impl NativeData for Server
{
    fn type_name(&self) -> &'static str
    {
        "__server_Server"
    }
    fn display(&self) -> String
    {
        format!("<HTTP server {}>", self.0.address)
    }
    fn as_any(&self) -> &dyn Any
    {
        self
    }
}

fn plain(status: u16, message: &str) -> Response<OutgoingBody>
{
    Response::builder()
        .status(status)
        .header("connection", "close")
        .body(
            Full::new(WireBytes::copy_from_slice(message.as_bytes()))
                .map_err(|error| match error {})
                .boxed_unsync(),
        )
        .unwrap()
}
fn encode_reply(reply: Reply) -> Result<Response<OutgoingBody>, NativeFault>
{
    let mut response = Response::builder()
        .status(reply.status)
        .header("connection", "close");
    let total: usize = reply
        .headers
        .iter()
        .map(|(name, values)| name.len() + values.iter().map(String::len).sum::<usize>())
        .sum();
    if reply.headers.len() > 256 || total > 65536
    {
        return Err(NativeFault::new(
            "LimitError",
            "Server response headers exceed their limit",
        ));
    }
    for (name, values) in reply.headers
    {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|error| NativeFault::new("OpenError", error.to_string()))?;
        if matches!(
            name.as_str(),
            "connection" | "transfer-encoding" | "content-length"
        )
        {
            return Err(NativeFault::new(
                "OpenError",
                "Server transport owns HTTP framing headers",
            ));
        }
        for value in values
        {
            response = response.header(
                name.clone(),
                HeaderValue::from_str(&value)
                    .map_err(|error| NativeFault::new("OpenError", error.to_string()))?,
            );
        }
    }
    let body = match reply.body
    {
        Outgoing::Empty => Full::new(WireBytes::new())
            .map_err(|error| match error {})
            .boxed_unsync(),
        Outgoing::Bytes(bytes) => Full::new(WireBytes::copy_from_slice(&bytes.0))
            .map_err(|error| match error {})
            .boxed_unsync(),
        Outgoing::Reader(_) | Outgoing::Source(_) =>
        {
            let (reader, source) = match reply.body
            {
                Outgoing::Reader(reader) => (reader.leased()?, None),
                Outgoing::Source(source) => (source.0.reader.borrowed(), Some(source)),
                _ => unreachable!(),
            };
            StreamBody::new(try_unfold(
                (reader, source),
                |(reader, source)| async move {
                    match reader.read(65536).await
                    {
                        Ok(Some(bytes)) => Ok(Some((
                            Frame::data(WireBytes::copy_from_slice(&bytes.0)),
                            (reader, source),
                        ))),
                        Ok(None) =>
                        {
                            reader.close();
                            if let Some(source) = source
                            {
                                source.close();
                            }
                            Ok(None)
                        }
                        Err(error) => Err(
                            if let Some(id) = error.script_error
                            {
                                io::Error::other(crate::runtime::tasks::ScriptFault(id))
                            }
                            else
                            {
                                io::Error::other(error.message)
                            },
                        ),
                    }
                },
            ))
            .boxed_unsync()
        }
    };
    response
        .body(body)
        .map_err(|error| NativeFault::new("OpenError", error.to_string()))
}
