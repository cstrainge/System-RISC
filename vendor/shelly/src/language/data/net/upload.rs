
use std::{
    any::Any,
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, Mutex},
};

use tokio::{sync::oneshot, task::AbortHandle};

use crate::{
    language::data::{
        io::{
            adapters::ByteSink,
            handles::{Control, Reader, Writer},
        },
        native_value::NativeData,
        net::http::{Client, Request, RequestBody, Response},
    },
    runtime::tasks::NativeFault,
};

struct State
{
    sink: ByteSink,
    reply: Mutex<Option<oneshot::Receiver<Result<Response, NativeFault>>>>,
    cached: Mutex<Option<Result<Response, NativeFault>>>,
    abort: AbortHandle,
    closed: AtomicBool,
}
#[derive(Clone)]
pub struct Upload(Arc<State>);
impl Upload
{
    pub async fn new(client: Client, mut request: Request) -> Result<Self, NativeFault>
    {
        let (input, output) = tokio::io::duplex(65536);
        let reader = Reader::new(input, Control::new());
        let writer = Writer::new(output, Control::new());
        let sink = ByteSink::new(writer, true)?;
        request.body = RequestBody::Reader(reader);
        let (sender, reply) = oneshot::channel();
        let producer = sink.clone();
        let task = tokio::spawn(async move {
            let result = client.send(request).await;
            // Headers can complete before upstream EOF. Wake a blocked producer and let the
            // pipeline preserve this response instead of manufacturing a write failure.
            let _ = sender.send(result);
            producer.close();
        });
        Ok(Self(Arc::new(State {
            sink,
            reply: Mutex::new(Some(reply)),
            cached: Mutex::new(None),
            abort: task.abort_handle(),
            closed: AtomicBool::new(false),
        })))
    }
    pub fn active(&self) -> bool
    {
        !self.0.closed.load(Ordering::Acquire)
    }
    pub fn sink(&self) -> ByteSink
    {
        self.0.sink.clone()
    }
    pub fn ready(&self) -> bool
    {
        if self.0.cached.lock().unwrap().is_some()
        {
            return true;
        }
        let mut reply = self.0.reply.lock().unwrap();
        if let Some(receiver) = reply.as_mut()
        {
            match receiver.try_recv()
            {
                Ok(result) =>
                {
                    *self.0.cached.lock().unwrap() = Some(result);
                    reply.take();
                    true
                }
                Err(oneshot::error::TryRecvError::Closed) =>
                {
                    *self.0.cached.lock().unwrap() = Some(Err(NativeFault::new(
                        "CancelledError",
                        "Upload operation was cancelled",
                    )));
                    reply.take();
                    true
                }
                Err(oneshot::error::TryRecvError::Empty) => false,
            }
        }
        else
        {
            false
        }
    }
    pub async fn finish(&self) -> Result<Response, NativeFault>
    {
        self.0.sink.close();
        if let Some(result) = self.0.cached.lock().unwrap().clone()
        {
            self.0.closed.store(true, Ordering::Release);
            return result;
        }
        let receiver = self.0.reply.lock().unwrap().take().ok_or_else(|| {
            NativeFault::new("LeaseError", "Upload already has a finishing consumer")
        })?;
        let result = receiver
            .await
            .map_err(|_| NativeFault::new("CancelledError", "Upload was cancelled"))?;
        *self.0.cached.lock().unwrap() = Some(result.clone());
        self.0.closed.store(true, Ordering::Release);
        result
    }
    pub fn close(&self)
    {
        if !self.0.closed.swap(true, Ordering::AcqRel)
        {
            self.0.sink.close();
            self.0.abort.abort();
            if let Some(Ok(response)) = self.0.cached.lock().unwrap().as_ref()
            {
                response.close();
            }
        }
    }
}
impl Drop for State
{
    fn drop(&mut self)
    {
        self.sink.close();
        self.abort.abort();
    }
}
impl NativeData for Upload
{
    fn type_name(&self) -> &'static str
    {
        "__http_UploadTransport"
    }
    fn display(&self) -> String
    {
        "<HTTP upload>".into()
    }
    fn as_any(&self) -> &dyn Any
    {
        self
    }
}
