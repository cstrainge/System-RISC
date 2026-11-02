//! Allocation-free entropy transport shared by std and the public SDK.
use mac_abi::entropy as wire;

use crate::{Client, Error, types::InlineText};

#[derive(Clone, Copy, Debug)]
pub enum Failure
{
    Resident
    {
        cause:  Error,
        detail: InlineText<{ wire::DETAIL_BYTES }>
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
impl core::fmt::Display for Failure
{
    fn fmt(&self, out: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::Resident { cause,
                             detail } => write!(out, "entropy unavailable: {cause}: {detail}"),
            Self::Protocol(detail) => write!(out, "entropy protocol refusal: {detail}")
        }
    }
}
impl core::error::Error for Failure
{
}

/// A failed multi-call fill leaves exactly this many leading bytes initialized.
/// The caller must discard the requested random value on any error.
#[derive(Clone, Copy, Debug)]
pub struct FillFailure
{
    failure:     Failure,
    initialized: usize
}
impl FillFailure
{
    pub fn failure(&self) -> &Failure
    {
        &self.failure
    }

    pub fn initialized(&self) -> usize
    {
        self.initialized
    }
}
impl core::fmt::Display for FillFailure
{
    fn fmt(&self, out: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        write!(out, "entropy fill stopped after {} bytes: {}", self.initialized, self.failure)
    }
}
impl core::error::Error for FillFailure
{
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)>
    {
        Some(&self.failure)
    }
}

impl Client<'_>
{
    /// Establish actual source readiness. This is not a provider lifetime lease
    /// and later fills still report genuine loss/unavailability.
    pub fn entropy_ready(self) -> core::result::Result<(), Failure>
    {
        call(self, &mut wire::Call::default()).map(|_| ())
    }

    /// Return the number of actual source bytes copied. An empty output is a
    /// local no-op; a nonempty successful call always makes forward progress.
    pub fn entropy_fill_some(self, output: &mut [u8]) -> core::result::Result<usize, Failure>
    {
        if output.is_empty()
        {
            return Ok(0);
        }
        let capacity = output.len().min(wire::MAX_CAPACITY);
        let mut request = wire::Call { operation: wire::FILL,
                                       output: output.as_mut_ptr(),
                                       capacity: capacity as u32,
                                       ..Default::default() };
        call(self, &mut request)
    }

    /// Fill the whole output through real bounded completions. No source bytes
    /// are synthesized, repeated after partial progress, or logged.
    pub fn entropy_fill(self, output: &mut [u8]) -> core::result::Result<(), FillFailure>
    {
        let mut initialized = 0;
        while initialized < output.len()
        {
            initialized += self.entropy_fill_some(&mut output[initialized..])
                               .map_err(|failure| {
                                   FillFailure { failure,
                                                 initialized }
                               })?;
        }
        Ok(())
    }
}

fn call(client: Client<'_>, call: &mut wire::Call) -> core::result::Result<usize, Failure>
{
    let original = *call;
    let status = client.invoke(mac_abi::traps::ENTROPY, call);
    if call.operation != original.operation
       || call.flags != original.flags
       || call.reserved != original.reserved
       || call.output != original.output
       || call.capacity != original.capacity
       || call.reserved2 != original.reserved2
    {
        return Err(Failure::Protocol("service changed input fields"));
    }
    let status: i32 =
        status.try_into().map_err(|_| Failure::Protocol("status exceeds native width"))?;
    if status == mac_abi::OK
    {
        if call.detail_length != 0
           || (call.operation == wire::READY && call.written != 0)
           || (call.operation == wire::FILL && (call.written == 0 || call.written > call.capacity))
        {
            return Err(Failure::Protocol("invalid successful byte count or diagnostic"));
        }
        return Ok(call.written as usize);
    }
    if status > 0 || call.written != 0
    {
        return Err(Failure::Protocol("refusal reported success or copied bytes"));
    }
    let detail = InlineText::from_reply(call.detail, call.detail_length)
                     .map_err(|_| Failure::Protocol("refusal detail is not bounded UTF-8"))?;
    if detail.as_str().is_empty()
    {
        return Err(Failure::Protocol("refusal omitted its cause"));
    }
    Err(Failure::Resident { cause: Error::from_status(status),
                            detail })
}
