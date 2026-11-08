
use std::{
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use tokio::{
    io::{AsyncRead, ReadBuf},
    sync::mpsc::{Receiver, Sender, channel},
};

use crate::{
    language::data::io::handles::{Control, Reader},
    runtime::tasks::ScriptFault,
};

pub enum Chunk
{
    Bytes(Arc<[u8]>),
    Failed(u64),
}

struct QueueReader
{
    receiver: Receiver<Chunk>,
    current: Arc<[u8]>,
    offset: usize,
}

impl AsyncRead for QueueReader
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>>
    {
        loop
        {
            if self.offset < self.current.len()
            {
                let count = output.remaining().min(self.current.len() - self.offset);
                output.put_slice(&self.current[self.offset..self.offset + count]);
                self.offset += count;
                return Poll::Ready(Ok(()));
            }
            match self.receiver.poll_recv(context)
            {
                Poll::Ready(Some(Chunk::Bytes(bytes))) =>
                {
                    self.current = bytes;
                    self.offset = 0;
                }
                Poll::Ready(Some(Chunk::Failed(id))) =>
                {
                    return Poll::Ready(Err(io::Error::other(ScriptFault(id))));
                }
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

pub fn reader() -> (Reader, Sender<Chunk>)
{
    let (sender, receiver) = channel(1);
    let reader = Reader::new(
        QueueReader {
            receiver,
            current: Arc::from([]),
            offset: 0,
        },
        Control::new(),
    );
    (reader, sender)
}
