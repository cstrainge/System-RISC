use alloc::vec::Vec;
use core::future::Future;

use mac_host_protocol::{Arguments, Header, Operation};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause
{
    Host(i32),
    Transport(i32),
    Format(mac_fork_file::Error),
    Invalid,
    NotFound,
    Stale,
    Conflict,
    ReadOnly,
    NoMemory,
    Busy,
    Unsupported,
    Cancelled
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error
{
    pub cause:   Cause,
    pub cleanup: Option<Cause>
}
impl From<Cause> for Error
{
    fn from(cause: Cause) -> Self
    {
        Self { cause,
               cleanup: None }
    }
}
impl From<mac_fork_file::Error> for Error
{
    fn from(error: mac_fork_file::Error) -> Self
    {
        Cause::Format(error).into()
    }
}
impl core::fmt::Display for Error
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        write!(f, "{self:?}")
    }
}
impl core::error::Error for Error
{
}
pub type Result<T> = core::result::Result<T, Error>;
#[derive(Debug)]
pub struct Reply
{
    pub header: Header,
    pub body:   Vec<u8>
}
/// Each call owns its progress while pending and retains no caller pointers
/// beyond the future's Rust borrow. Native implementations advance bounded work
/// per poll. Dropping a module operation must drain/cancel its lower request.
pub trait Rpc: Send
{
    fn invoke(&mut self, operation: Operation, arguments: Arguments<'_>)
              -> impl Future<Output = Result<Reply>> + Send;
}
/// Yield even if a test or an unusually fast transport completes immediately.
/// A scan/hash/copy loop can never monopolize one module callback.
pub(crate) async fn yield_once()
{
    let mut yielded = false;
    core::future::poll_fn(|cx| {
        if yielded
        {
            core::task::Poll::Ready(())
        }
        else
        {
            yielded = true;
            cx.waker().wake_by_ref();
            core::task::Poll::Pending
        }
    }).await;
}
pub(crate) fn buffer(length: usize) -> Result<Vec<u8>>
{
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(length).map_err(|_| Cause::NoMemory)?;
    bytes.resize(length, 0);
    Ok(bytes)
}
