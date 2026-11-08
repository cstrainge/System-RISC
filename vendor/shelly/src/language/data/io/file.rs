
use std::{
    fs::File as OsFile,
    future::Future,
    io::{self, Read, Write},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::runtime::{blocking, tasks::NativeFault};

type ReadJob = Pin<Box<dyn Future<Output = Result<io::Result<Vec<u8>>, NativeFault>> + Send>>;
type WriteJob = Pin<Box<dyn Future<Output = Result<io::Result<()>, NativeFault>> + Send>>;
pub struct File
{
    file: Arc<Mutex<OsFile>>,
    read: Option<ReadJob>,
    write: Option<WriteJob>,
    input: Vec<u8>,
    offset: usize,
}
impl File
{
    pub fn new(file: OsFile) -> Self
    {
        Self {
            file: Arc::new(Mutex::new(file)),
            read: None,
            write: None,
            input: Vec::new(),
            offset: 0,
        }
    }
}
fn fault(error: NativeFault) -> io::Error
{
    io::Error::new(
        if error.kind == "LimitError"
        {
            io::ErrorKind::WouldBlock
        }
        else
        {
            io::ErrorKind::Other
        },
        error.message,
    )
}
impl AsyncRead for File
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>>
    {
        if buffer.remaining() == 0
        {
            return Poll::Ready(Ok(()));
        }
        if self.offset < self.input.len()
        {
            let count = buffer.remaining().min(self.input.len() - self.offset);
            buffer.put_slice(&self.input[self.offset..self.offset + count]);
            self.offset += count;
            return Poll::Ready(Ok(()));
        }
        if self.read.is_none()
        {
            let file = self.file.clone();
            let maximum = buffer.remaining().min(65536);
            self.read = Some(Box::pin(blocking::run(move || {
                let mut bytes = vec![0; maximum];
                let count = file.lock().unwrap().read(&mut bytes)?;
                bytes.truncate(count);
                Ok(bytes)
            })));
        }
        match self.read.as_mut().unwrap().as_mut().poll(context)
        {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) =>
            {
                self.read = None;
                let bytes = match result
                {
                    Ok(Ok(bytes)) => bytes,
                    Ok(Err(error)) => return Poll::Ready(Err(error)),
                    Err(error) => return Poll::Ready(Err(fault(error))),
                };
                if bytes.is_empty()
                {
                    return Poll::Ready(Ok(()));
                }
                self.input = bytes;
                self.offset = 0;
                self.poll_read(context, buffer)
            }
        }
    }
}
impl AsyncWrite for File
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>>
    {
        if let Some(write) = &mut self.write
        {
            match write.as_mut().poll(context)
            {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(result) =>
                {
                    self.write = None;
                    match result
                    {
                        Ok(Ok(())) =>
                        {}
                        Ok(Err(error)) => return Poll::Ready(Err(error)),
                        Err(error) => return Poll::Ready(Err(fault(error))),
                    }
                }
            }
        }
        let count = bytes.len().min(65536);
        let bytes = bytes[..count].to_vec();
        let file = self.file.clone();
        self.write = Some(Box::pin(blocking::run(move || {
            file.lock().unwrap().write_all(&bytes)
        })));
        // Poll once to submit the owned buffer before reporting its accepted count.
        if let Some(write) = &mut self.write
        {
            match write.as_mut().poll(context)
            {
                Poll::Ready(Ok(Err(error))) =>
                {
                    self.write = None;
                    return Poll::Ready(Err(error));
                }
                Poll::Ready(Err(error)) =>
                {
                    self.write = None;
                    return Poll::Ready(Err(fault(error)));
                }
                Poll::Ready(Ok(Ok(()))) => self.write = None,
                Poll::Pending =>
                {}
            }
        }
        Poll::Ready(Ok(count))
    }
    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>>
    {
        if let Some(write) = &mut self.write
        {
            match write.as_mut().poll(context)
            {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(result) =>
                {
                    self.write = None;
                    return Poll::Ready(result.map_err(fault).and_then(|result| result));
                }
            }
        }
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>>
    {
        self.poll_flush(context)
    }
}
