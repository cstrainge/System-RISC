
use std::{
    any::Any,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use tokio::sync::Mutex;

use crate::{
    language::data::{
        io::{Bytes, handles::Reader},
        native_value::NativeData,
    },
    runtime::tasks::NativeFault,
};

struct Buffer
{
    bytes: Vec<u8>,
    skip_lf: bool,
    eof: bool,
    terminated: bool,
}
struct State
{
    reader: Reader,
    buffer: Mutex<Buffer>,
    busy: AtomicBool,
    closed: AtomicBool,
    maximum: usize,
    universal: bool,
    separator: Option<u8>,
}

#[derive(Clone)]
pub struct Lines(Arc<State>);

impl Lines
{
    pub fn separated(
        reader: Reader,
        maximum: usize,
        universal: bool,
        separator: Option<u8>,
    ) -> Result<Self, NativeFault>
    {
        Ok(Self(Arc::new(State {
            reader: reader.leased()?,
            buffer: Mutex::new(Buffer {
                bytes: Vec::new(),
                skip_lf: false,
                eof: false,
                terminated: false,
            }),
            busy: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            maximum,
            universal,
            separator,
        })))
    }
    pub fn close(&self)
    {
        self.0.closed.store(true, Ordering::Release);
        self.0.reader.close();
    }
    pub async fn next(&self) -> Result<Option<Bytes>, NativeFault>
    {
        if self.0.busy.swap(true, Ordering::AcqRel)
        {
            return Err(NativeFault::new(
                "LeaseError",
                "Line reader already has a consumer",
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
        let mut buffer = self.0.buffer.lock().await;
        loop
        {
            if buffer.skip_lf && !buffer.bytes.is_empty()
            {
                if buffer.bytes[0] == b'\n'
                {
                    buffer.bytes.remove(0);
                }
                buffer.skip_lf = false;
            }
            if let Some(end) = buffer.bytes.iter().position(|byte| {
                self.0.separator.map_or(
                    *byte == b'\n' || self.0.universal && *byte == b'\r',
                    |separator| *byte == separator,
                )
            })
            {
                if end > self.0.maximum
                {
                    return Err(NativeFault::new(
                        "LimitError",
                        "Physical line exceeds its byte limit",
                    ));
                }
                let tail = buffer.bytes.split_off(end + 1);
                let mut line = std::mem::replace(&mut buffer.bytes, tail);
                buffer.terminated = true;
                buffer.skip_lf = line.pop() == Some(b'\r') && self.0.separator.is_none();
                if self.0.separator.is_none() && !self.0.universal && line.last() == Some(&b'\r')
                {
                    line.pop();
                }
                return Ok(Some(Bytes(line.into())));
            }
            if buffer.bytes.len() > self.0.maximum
            {
                return Err(NativeFault::new(
                    "LimitError",
                    "Physical line exceeds its byte limit",
                ));
            }
            if buffer.eof
            {
                if buffer.bytes.is_empty()
                {
                    if self.0.separator.is_some() && buffer.terminated
                    {
                        buffer.terminated = false;
                        return Ok(Some(Bytes(Arc::from(&b""[..]))));
                    }
                    return Ok(None);
                }
                buffer.terminated = false;
                return Ok(Some(Bytes(std::mem::take(&mut buffer.bytes).into())));
            }
            match self.0.reader.read(4096).await?
            {
                Some(bytes) => buffer.bytes.extend_from_slice(&bytes.0),
                None =>
                {
                    buffer.eof = true;
                    self.0.reader.close();
                }
            }
        }
    }
}

impl NativeData for Lines
{
    fn operation(&self) -> bool
    {
        !self.0.closed.load(Ordering::Acquire)
    }
    fn release(&self)
    {
        self.close();
    }
    fn type_name(&self) -> &'static str
    {
        "__io_Lines"
    }
    fn display(&self) -> String
    {
        "<Line reader>".into()
    }
    fn as_any(&self) -> &dyn Any
    {
        self
    }
}
