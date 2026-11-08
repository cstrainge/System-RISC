
use std::{
    any::Any,
    io,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use futures_util::{StreamExt, stream::try_unfold};

use reqwest::{
    Client as Transport, Method, Response as TransportResponse, Url,
    header::{HeaderMap, HeaderName, HeaderValue},
    redirect::Policy,
};

use tokio::{
    io::{AsyncRead, ReadBuf},
    sync::Notify,
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

pub struct ClientState
{
    pub transport: Mutex<Option<Transport>>,
    pub base: Url,
    pub timeout: Option<Duration>,
    pub authenticated: bool,
    pub closed: AtomicBool,
    pub wake: Notify,
    bodies: Mutex<Vec<std::sync::Weak<Control>>>,
    open: AtomicUsize,
}

#[derive(Clone)]
pub struct Client(pub Arc<ClientState>);

struct Permit(Arc<ClientState>);
impl Drop for Permit
{
    fn drop(&mut self)
    {
        self.0.open.fetch_sub(1, Ordering::AcqRel);
    }
}

struct ManagedRead<R>
{
    inner: R,
    _permit: Permit,
}
impl<R: AsyncRead + Unpin> AsyncRead for ManagedRead<R>
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>>
    {
        Pin::new(&mut self.inner).poll_read(context, buffer)
    }
}

pub enum RequestBody
{
    Empty,
    Bytes(Bytes),
    Reader(Reader),
    Source(ByteSource),
}

pub struct Request
{
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, Vec<String>)>,
    pub query: Vec<(String, String)>,
    pub body: RequestBody,
    pub timeout: Option<Option<Duration>>,
}

pub struct ResponseState
{
    pub status: u16,
    pub deadline: Option<std::time::Instant>,
    pub headers: Vec<(String, Vec<String>)>,
    pub url: String,
    pub body: Mutex<Option<Reader>>,
    pub diagnostic: Mutex<Option<Diagnostic>>,
    pub control: Arc<Control>,
}

#[derive(Clone)]
pub struct Response(pub Arc<ResponseState>);

#[derive(Clone)]
pub struct Diagnostic
{
    pub status: u16,
    pub deadline: Option<std::time::Instant>,
    pub headers: Vec<(String, Vec<String>)>,
    pub url: String,
    pub bytes: Bytes,
    pub complete: bool,
    pub bytes_received: usize,
    pub reason: String,
}

fn headers(values: Vec<(String, Vec<String>)>) -> Result<HeaderMap, NativeFault>
{
    let mut headers = HeaderMap::new();
    if values.len() > 256
    {
        return Err(NativeFault::new("LimitError", "Too many HTTP headers"));
    }
    let total: usize = values
        .iter()
        .map(|(name, values)| name.len() + values.iter().map(String::len).sum::<usize>())
        .sum();
    if total > 65536 || values.iter().map(|(_, values)| values.len()).sum::<usize>() > 1024
    {
        return Err(NativeFault::new(
            "LimitError",
            "HTTP headers exceed their byte/value limit",
        ));
    }
    for (name, values) in values
    {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| NativeFault::new("OpenError", "Invalid HTTP header name"))?;
        for value in values
        {
            if value.len() > 65536
            {
                return Err(NativeFault::new(
                    "LimitError",
                    "HTTP header value is too large",
                ));
            }
            headers.append(
                name.clone(),
                HeaderValue::from_str(&value)
                    .map_err(|_| NativeFault::new("OpenError", "Invalid HTTP header value"))?,
            );
        }
    }
    for singleton in [
        "authorization",
        "proxy-authorization",
        "host",
        "content-type",
        "content-length",
    ]
    {
        if headers.get_all(singleton).iter().count() > 1
        {
            return Err(NativeFault::new(
                "OpenError",
                "HTTP singleton header has multiple values",
            ));
        }
    }
    Ok(headers)
}

fn response_headers(response: &TransportResponse)
-> Result<Vec<(String, Vec<String>)>, NativeFault>
{
    response
        .headers()
        .keys()
        .map(|name| {
            let values = response
                .headers()
                .get_all(name)
                .iter()
                .map(|value| {
                    std::str::from_utf8(value.as_bytes())
                        .map(str::to_string)
                        .map_err(|_| {
                            NativeFault::new("DecodeError", "HTTP header value is not UTF-8")
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok((name.as_str().into(), values))
        })
        .collect()
}

impl Client
{
    pub fn new(
        base: &str,
        defaults: Vec<(String, Vec<String>)>,
        timeout: Option<Duration>,
        connect: Duration,
        idle: Option<Duration>,
    ) -> Result<Self, NativeFault>
    {
        let base =
            Url::parse(base).map_err(|_| NativeFault::new("OpenError", "Invalid HTTP base URL"))?;
        if !matches!(base.scheme(), "http" | "https")
            || !base.username().is_empty()
            || base.password().is_some()
        {
            return Err(NativeFault::new(
                "OpenError",
                "HTTP base URL must use http/https without user information",
            ));
        }
        let defaults = headers(defaults)?;
        let authenticated = ["authorization", "cookie", "proxy-authorization"]
            .iter()
            .any(|name| defaults.contains_key(*name));
        let mut builder = Transport::builder()
            .retry(reqwest::retry::never())
            .default_headers(defaults)
            .connect_timeout(connect)
            .redirect(Policy::custom(|attempt| {
                if attempt.previous().len() >= 10
                {
                    return attempt.error("HTTP redirect limit exceeded");
                }
                if attempt.previous().last().is_some_and(|previous| {
                    previous.scheme() == "https" && attempt.url().scheme() != "https"
                })
                {
                    return attempt.error("HTTPS redirect downgrade rejected");
                }
                attempt.follow()
            }));
        if let Some(idle) = idle
        {
            builder = builder.read_timeout(idle);
        }
        let transport = builder
            .build()
            .map_err(|error| NativeFault::new("OpenError", error.to_string()))?;
        Ok(Self(Arc::new(ClientState {
            transport: Mutex::new(Some(transport)),
            base,
            timeout,
            authenticated,
            closed: AtomicBool::new(false),
            wake: Notify::new(),
            bodies: Mutex::new(Vec::new()),
            open: AtomicUsize::new(0),
        })))
    }

    pub fn close(&self)
    {
        self.0.closed.store(true, Ordering::Release);
        self.0.wake.notify_waiters();
        self.0.transport.lock().unwrap().take();
        for body in self
            .0
            .bodies
            .lock()
            .unwrap()
            .iter()
            .filter_map(std::sync::Weak::upgrade)
        {
            body.close();
        }
    }

    pub async fn send(&self, request: Request) -> Result<Response, NativeFault>
    {
        let timeout = request.timeout.unwrap_or(self.0.timeout);
        let deadline = timeout.map(|timeout| std::time::Instant::now() + timeout);
        if self.0.closed.load(Ordering::Acquire)
        {
            return Err(NativeFault::new("ClosedError", "HTTP client is closed"));
        }
        if self.0.open.fetch_add(1, Ordering::AcqRel) >= 64
        {
            self.0.open.fetch_sub(1, Ordering::AcqRel);
            return Err(NativeFault::new(
                "LimitError",
                "HTTP client has 64 unconsumed bodies",
            ));
        }
        let permit = Permit(self.0.clone());
        let transport = self
            .0
            .transport
            .lock()
            .unwrap()
            .as_ref()
            .cloned()
            .ok_or_else(|| NativeFault::new("ClosedError", "HTTP client is closed"))?;
        let mut url = self
            .0
            .base
            .join(&request.path)
            .map_err(|_| NativeFault::new("OpenError", "Invalid HTTP URL reference"))?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(NativeFault::new(
                "OpenError",
                "Unsupported HTTP URL or embedded credentials",
            ));
        }
        let headers = headers(request.headers)?;
        let authenticated = self.0.authenticated
            || ["authorization", "cookie", "proxy-authorization"]
                .iter()
                .any(|name| headers.contains_key(*name));
        if authenticated && url.origin() != self.0.base.origin()
        {
            return Err(NativeFault::new(
                "OpenError",
                "Credential-bearing cross-origin request rejected",
            ));
        }
        if self.0.base.scheme() == "https" && url.scheme() != "https"
        {
            return Err(NativeFault::new(
                "OpenError",
                "HTTPS request downgrade rejected",
            ));
        }
        for (key, value) in request.query
        {
            url.query_pairs_mut().append_pair(&key, &value);
        }
        let method = Method::from_bytes(request.method.as_bytes())
            .map_err(|_| NativeFault::new("OpenError", "Invalid HTTP method"))?;
        let mut builder = transport.request(method, url).headers(headers);
        if let Some(timeout) = timeout
        {
            builder = builder.timeout(timeout);
        }
        builder = match request.body
        {
            RequestBody::Empty => builder,
            RequestBody::Bytes(bytes) => builder.body(bytes.0.to_vec()),
            RequestBody::Reader(_) | RequestBody::Source(_) =>
            {
                let (reader, source) = match request.body
                {
                    RequestBody::Reader(reader) => (reader, None),
                    RequestBody::Source(source) => (source.0.reader.borrowed(), Some(source)),
                    _ => unreachable!(),
                };
                let reader = reader.leased()?;
                let stream = try_unfold((reader, source), |(reader, source)| async move {
                    match reader.read(65536).await
                    {
                        Ok(Some(bytes)) => Ok(Some((bytes.0.to_vec(), (reader, source)))),
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
                });
                builder.body(reqwest::Body::wrap_stream(stream))
            }
        };
        let cancelled = self.0.wake.notified();
        tokio::pin!(cancelled);
        cancelled.as_mut().enable();
        if self.0.closed.load(Ordering::Acquire)
        {
            return Err(NativeFault::new("ClosedError", "HTTP client is closed"));
        }
        let response = tokio::select! {
            response = builder.send() => response.map_err(|error| NativeFault::error(
                if error.is_timeout() { "TimeoutError" } else { "ReadError" }, &error.without_url()))?,
            _ = &mut cancelled => return Err(NativeFault::new("CancelledError", "HTTP client closed during request")),
        };
        let status = response.status().as_u16();
        let headers = response_headers(&response)?;
        let url = response.url().to_string();
        let stream = response
            .bytes_stream()
            .map(|result| result.map_err(|error| io::Error::other(error.without_url())));
        let control = Control::new();
        {
            let mut bodies = self.0.bodies.lock().unwrap();
            bodies.retain(|body| body.strong_count() != 0);
            bodies.push(Arc::downgrade(&control));
        }
        let reader = Reader::new(
            ManagedRead {
                inner: StreamReader::new(stream),
                _permit: permit,
            },
            control.clone(),
        );
        Ok(Response(Arc::new(ResponseState {
            status,
            deadline,
            headers,
            url,
            body: Mutex::new(Some(reader)),
            diagnostic: Mutex::new(None),
            control,
        })))
    }
}

impl Response
{
    pub fn take_body(&self) -> Result<Reader, NativeFault>
    {
        self.0
            .body
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| NativeFault::new("ClosedError", "HTTP body was already taken"))
    }
    pub fn close(&self)
    {
        self.0.control.close();
        if let Some(reader) = self.0.body.lock().unwrap().take()
        {
            reader.close_underlying();
        }
    }
    pub async fn capture(&self) -> Result<Diagnostic, NativeFault>
    {
        if let Some(diagnostic) = self.0.diagnostic.lock().unwrap().clone()
        {
            return Ok(diagnostic);
        }
        let reader = self.take_body()?;
        let mut bytes = Vec::new();
        let mut received = 0usize;
        let capture = async {
            loop
            {
                match reader.read(65537 - bytes.len()).await?
                {
                    Some(chunk) =>
                    {
                        received += chunk.0.len();
                        bytes.extend_from_slice(&chunk.0);
                    }
                    None => return Ok::<_, NativeFault>((true, "Complete")),
                }
                if bytes.len() > 65536
                {
                    bytes.truncate(65536);
                    return Ok((false, "Limit"));
                }
            }
        };
        let remaining = self
            .0
            .deadline
            .map_or(Duration::from_millis(500), |deadline| {
                deadline
                    .saturating_duration_since(std::time::Instant::now())
                    .min(Duration::from_millis(500))
            });
        let result = tokio::time::timeout(remaining, capture).await;
        reader.close_underlying();
        let (complete, reason) = match result
        {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => (false, "ReadError"),
            Err(_) => (false, "Timeout"),
        };
        let mut diagnostic_url = Url::parse(&self.0.url).expect("Response URL was validated");
        diagnostic_url.set_query(None);
        diagnostic_url.set_fragment(None);
        let diagnostic = Diagnostic {
            status: self.0.status,
            deadline: self.0.deadline,
            headers: self.0.headers.clone(),
            url: diagnostic_url.to_string(),
            bytes: Bytes(bytes.into()),
            complete,
            bytes_received: received,
            reason: reason.into(),
        };
        *self.0.diagnostic.lock().unwrap() = Some(diagnostic.clone());
        Ok(diagnostic)
    }
}

impl NativeData for Client
{
    fn type_name(&self) -> &'static str
    {
        "__http_Client"
    }
    fn display(&self) -> String
    {
        "<HTTP client>".into()
    }
    fn as_any(&self) -> &dyn Any
    {
        self
    }
}
impl NativeData for Response
{
    fn type_name(&self) -> &'static str
    {
        "__http_Response"
    }
    fn display(&self) -> String
    {
        format!("<HTTP {}>", self.0.status)
    }
    fn as_any(&self) -> &dyn Any
    {
        self
    }
}
impl NativeData for Diagnostic
{
    fn type_name(&self) -> &'static str
    {
        "__http_DiagnosticResponse"
    }
    fn display(&self) -> String
    {
        format!("<HTTP {} {}>", self.status, self.reason)
    }
    fn as_any(&self) -> &dyn Any
    {
        self
    }
}
