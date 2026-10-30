//! Core-only native thread/TLS transport for the platform CRT. Resident code
//! authenticates the actual Process and THRD; copied IDs grant no authority.
use core::fmt;

use mac_abi::{native_threads as thread, tls};

use crate::{Client, Error, ThreadId, types::InlineText};
#[derive(Clone, Copy, Debug)]
pub enum Failure
{
    Resident
    {
        cause:  Error,
        detail: InlineText<192>
    },
    Protocol(&'static str)
}
impl Failure
{
    pub fn cause(&self) -> Error
    {
        match self
        {
            Self::Resident { cause,
                             .. } => *cause,
            Self::Protocol(_) => Error::BadAbi
        }
    }
}
impl fmt::Display for Failure
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Resident { cause,
                             detail } => write!(out, "{cause}: {detail}"),
            Self::Protocol(detail) => write!(out, "native thread/TLS protocol refusal: {detail}")
        }
    }
}
impl core::error::Error for Failure
{
}
pub type Result<T> = core::result::Result<T, Failure>;
pub(crate) fn result(status: i64, bytes: [u8; 192], length: u32) -> Result<()>
{
    let status: mac_abi::Status =
        status.try_into().map_err(|_| Failure::Protocol("status exceeds Status width"))?;
    if status == mac_abi::OK
    {
        return if length == 0
        {
            Ok(())
        }
        else
        {
            Err(Failure::Protocol("success returned failure detail"))
        };
    }
    if status > 0
    {
        return Err(Failure::Protocol("positive status is not a successful result"));
    }
    let detail = InlineText::from_reply(bytes, length).map_err(|_| {
                                                          Failure::Protocol("resident detail is \
                                                                             not bounded UTF-8")
                                                      })?;
    if detail.as_str().is_empty()
    {
        return Err(Failure::Protocol("refusal omitted its typed diagnostic"));
    }
    Err(Failure::Resident { cause: Error::from_status(status),
                            detail })
}
impl Client<'_>
{
    pub fn current_native_thread(self) -> Result<ThreadId>
    {
        let mut call = thread::Call::default();
        let status = self.invoke(mac_abi::traps::NATIVE_THREAD, &mut call);
        if call.operation != thread::CURRENT || call.reserved != 0 || call.reserved2 != 0
        {
            return Err(Failure::Protocol("CURRENT changed input fields"));
        }
        result(status, call.detail, call.detail_length)?;
        if call.thread == 0
        {
            return Err(Failure::Protocol("CURRENT returned no canonical THRD"));
        }
        Ok(ThreadId(call.thread))
    }

    /// # Safety
    /// CREATE's nonzero callback has the C destructor signature and remains
    /// executable in this Process. Values are opaque. NEXT's callback must run
    /// only in this thread, before ACK; no callback may unwind across the ABI.
    /// The caller owns teardown ordering and must not use TLS after FINISH.
    pub unsafe fn tls(self, call: &mut tls::Call) -> Result<()>
    {
        let original = *call;
        let status = self.invoke(mac_abi::traps::TLS, call);
        if call.operation != original.operation
           || call.flags != original.flags
           || call.reserved != original.reserved
           || call.reserved2 != original.reserved2
           || call.present > 1
           || call.round > tls::DESTRUCTOR_ROUNDS
        {
            return Err(Failure::Protocol("TLS returned invalid control fields"));
        }
        result(status, call.detail, call.detail_length)
    }
}
