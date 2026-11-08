
use std::{
    any::Any,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use crate::{
    language::data::{
        io::handles::{Reader, Writer},
        native_value::NativeData,
    },
    runtime::tasks::NativeFault,
};

pub struct SourceState
{
    pub reader: Reader,
    pub owned: bool,
    pub closed: AtomicBool,
}

#[derive(Clone)]
pub struct ByteSource(pub Arc<SourceState>);

impl ByteSource
{
    pub fn new(reader: Reader, owned: bool) -> Result<Self, NativeFault>
    {
        Ok(Self(Arc::new(SourceState {
            reader: reader.leased()?,
            owned,
            closed: AtomicBool::new(false),
        })))
    }
    pub fn close(&self)
    {
        if !self.0.closed.swap(true, Ordering::AcqRel)
        {
            self.0.reader.close();
            if self.0.owned
            {
                self.0.reader.close_underlying();
            }
        }
    }
}
impl Drop for SourceState
{
    fn drop(&mut self)
    {
        self.reader.close();
        if self.owned
        {
            self.reader.close_underlying();
        }
    }
}
impl NativeData for ByteSource
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
        "__io_ByteSource"
    }
    fn display(&self) -> String
    {
        "<ByteSource>".into()
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

pub struct SinkState
{
    pub writer: Writer,
    pub owned: bool,
    pub closed: AtomicBool,
    pub count: AtomicU64,
}

#[derive(Clone)]
pub struct ByteSink(pub Arc<SinkState>);

impl ByteSink
{
    pub fn new(writer: Writer, owned: bool) -> Result<Self, NativeFault>
    {
        Ok(Self(Arc::new(SinkState {
            writer: writer.leased()?,
            owned,
            closed: AtomicBool::new(false),
            count: AtomicU64::new(0),
        })))
    }
    pub fn close(&self)
    {
        if !self.0.closed.swap(true, Ordering::AcqRel)
        {
            self.0.writer.close();
            if self.0.owned
            {
                self.0.writer.close_underlying();
            }
        }
    }
}
impl Drop for SinkState
{
    fn drop(&mut self)
    {
        self.writer.close();
        if self.owned
        {
            self.writer.close_underlying();
        }
    }
}
impl NativeData for ByteSink
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
        "__io_ByteSink"
    }
    fn display(&self) -> String
    {
        "<ByteSink>".into()
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
