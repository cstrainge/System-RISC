//! Bounded raw channel policy, independent of MMIO for host workflow tests.
#![forbid(unsafe_code)]
use mac_abi::{BAD_ARGUMENT, IO_ERROR, NOT_SUPPORTED, SERVICE_BUSY};
use mac_byte_channel::{self as wire, State, Status};
use mac_driver::{Completion, Error, Phase, Request, Result};
use mac_virtio_console::{Channel, Diagnostic};

pub trait Connection: Channel
{
    fn generation(&self) -> u64;
    fn diagnostic(&self) -> Diagnostic;
    fn unavailable(&self) -> bool;
}
#[cfg(target_arch = "riscv64")]
impl Connection for mac_virtio_console::Device
{
    fn generation(&self) -> u64
    {
        self.generation()
    }

    fn diagnostic(&self) -> Diagnostic
    {
        self.diagnostic()
    }

    fn unavailable(&self) -> bool
    {
        self.unavailable()
    }
}

pub fn request(channel: &mut impl Connection, request: Request<'_>, output: &mut [u8])
               -> Result<Completion>
{
    if request.class != wire::CLASS
    {
        return Err(Error(NOT_SUPPORTED));
    }
    if request.phase == Phase::Cancel
    {
        return Ok(Completion::Complete(0));
    }
    // The provider never returns Pending. A live Poll is therefore a first
    // dispatch retried after broker contention; its caller resupplies owned
    // input. The broker rejects already-completed/foreign ticket replay.
    match request.operation
    {
        wire::ERROR_TEXT =>
        {
            if !request.input.is_empty()
            {
                return Err(Error(BAD_ARGUMENT));
            }
            let diagnostic = channel.diagnostic();
            let message = diagnostic.message().as_bytes();
            let suffix: &[u8] = if matches!(diagnostic,
                                            Diagnostic::DiscoveryTimeout
                                            | Diagnostic::Profile
                                            | Diagnostic::NameMismatch
                                            | Diagnostic::NameTimeout
                                            | Diagnostic::NoController)
            {
                b"; required port: com.redhat.spice.0"
            }
            else
            {
                &[]
            };
            let length = message.len() + suffix.len();
            if output.len() < length || length > wire::MAX_ERROR_TEXT
            {
                return Err(Error(BAD_ARGUMENT));
            }
            output[..message.len()].copy_from_slice(message);
            output[message.len()..length].copy_from_slice(suffix);
            Ok(Completion::Complete(length))
        }
        wire::STATUS =>
        {
            if !request.input.is_empty() || output.len() < wire::STATUS_BYTES
            {
                return Err(Error(BAD_ARGUMENT));
            }
            let (state, last_status) = match channel.poll()
            {
                Ok(true) => (State::Open, 0),
                Ok(false) => (State::Discovering, 0),
                Err(Error(status)) =>
                {
                    let state = if channel.unavailable()
                    {
                        State::Unavailable
                    }
                    else if channel.diagnostic() == Diagnostic::Disconnected
                    {
                        State::Disconnected
                    }
                    else if status == SERVICE_BUSY
                    {
                        State::Discovering
                    }
                    else
                    {
                        State::Failed
                    };
                    (state, status)
                }
            };
            output[..wire::STATUS_BYTES].copy_from_slice(&Status { state,last_status,generation:channel.generation() }.encode());
            Ok(Completion::Complete(wire::STATUS_BYTES))
        }
        wire::READ =>
        {
            if !request.input.is_empty() || output.is_empty() || output.len() > wire::MAX_TRANSFER
            {
                return Err(Error(BAD_ARGUMENT));
            }
            if !channel.poll()?
            {
                return Err(Error(SERVICE_BUSY));
            }
            let count = channel.receive(output)?;
            if count > output.len()
            {
                return Err(Error(IO_ERROR));
            }
            Ok(Completion::Complete(count))
        }
        wire::WRITE =>
        {
            if request.input.is_empty()
               || request.input.len() > wire::MAX_TRANSFER
               || output.len() < 4
            {
                return Err(Error(BAD_ARGUMENT));
            }
            if !channel.poll()?
            {
                return Err(Error(SERVICE_BUSY));
            }
            let count = channel.send(request.input)?;
            if count > request.input.len()
            {
                return Err(Error(IO_ERROR));
            }
            output[..4].copy_from_slice(&(count as u32).to_le_bytes());
            Ok(Completion::Complete(4))
        }
        _ => Err(Error(NOT_SUPPORTED))
    }
}
