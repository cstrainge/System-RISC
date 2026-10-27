//! VBYT v1: a single declared named byte stream. STATUS advances discovery and
//! reports a checked connection generation; READ/WRITE never retain slices.
//! A new generation revokes all prior queued bytes and application correlation.
#![no_std]
#![forbid(unsafe_code)]
use mac_abi::{BAD_ABI, BAD_ARGUMENT};
use mac_driver::{ClassId, Client, Completion, Error, Phase, Provider, Result};
pub const CLASS: ClassId = ClassId(*b"VBYT");
pub const VERSION: u32 = 1;
pub const STATUS: u32 = 1;
pub const READ: u32 = 2;
pub const WRITE: u32 = 3;
pub const ERROR_TEXT: u32 = 4;
pub const MAX_TRANSFER: usize = 4096;
pub const STATUS_BYTES: usize = 16;
pub const MAX_ERROR_TEXT: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum State
{
    Discovering  = 0,
    Open         = 1,
    Disconnected = 2,
    Unavailable  = 3,
    Failed       = 4
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status
{
    pub state:       State,
    pub last_status: i32,
    pub generation:  u64
}
impl Status
{
    pub fn encode(self) -> [u8; STATUS_BYTES]
    {
        let mut bytes = [0; STATUS_BYTES];
        bytes[..4].copy_from_slice(&(self.state as u32).to_le_bytes());
        bytes[4..8].copy_from_slice(&self.last_status.to_le_bytes());
        bytes[8..].copy_from_slice(&self.generation.to_le_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self>
    {
        if bytes.len() != STATUS_BYTES
        {
            return Err(Error(BAD_ABI));
        }
        let state = match u32::from_le_bytes(bytes[..4].try_into().map_err(|_| Error(BAD_ABI))?)
        {
            0 => State::Discovering,
            1 => State::Open,
            2 => State::Disconnected,
            3 => State::Unavailable,
            4 => State::Failed,
            _ => return Err(Error(BAD_ABI))
        };
        let last_status = i32::from_le_bytes(bytes[4..8].try_into().map_err(|_| Error(BAD_ABI))?);
        let generation = u64::from_le_bytes(bytes[8..].try_into().map_err(|_| Error(BAD_ABI))?);
        if last_status > 0 || state == State::Open && generation == 0
        {
            return Err(Error(BAD_ABI));
        }
        Ok(Self { state,
                  last_status,
                  generation })
    }
}

/// Thin lifetime-preserving client. Pending broker dispatch retains the caller's
/// ticket: poll/cancel that exact ticket to terminal acknowledgement. WRITE's
/// four-byte reply is the accepted byte count; advance only that many bytes.
/// Broker contention can return SERVICE_BUSY with a nonzero ticket before the
/// provider receives input. Resupply the same owned WRITE bytes on that Poll.
pub struct ByteChannel
{
    provider: Provider
}
impl ByteChannel
{
    pub fn resolve(client: Client) -> Result<Self>
    {
        Ok(Self { provider: client.resolve(CLASS, VERSION)? })
    }

    pub fn invoke(&self, ticket: &mut u64, phase: Phase, operation: u32, input: &[u8],
                  output: &mut [u8])
                  -> Result<Completion>
    {
        if !matches!(operation, STATUS | READ | WRITE | ERROR_TEXT)
           || input.len() > MAX_TRANSFER
           || output.len() > MAX_TRANSFER
        {
            return Err(Error(BAD_ARGUMENT));
        }
        self.provider.invoke(ticket, phase, operation, input, output)
    }
}

pub fn accepted(bytes: &[u8], offered: usize) -> Result<usize>
{
    if bytes.len() != 4
    {
        return Err(Error(BAD_ABI));
    }
    let accepted = u32::from_le_bytes(bytes.try_into().map_err(|_| Error(BAD_ABI))?) as usize;
    if accepted > offered
    {
        return Err(Error(BAD_ABI));
    }
    Ok(accepted)
}
