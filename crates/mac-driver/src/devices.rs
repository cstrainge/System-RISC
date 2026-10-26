//! Thin checked access to the resident device/DMA service. Claims and mappings
//! remain pinned in System until explicit reset, unmap and release succeed.
//! Hardware adapters retain their DMA storage throughout pending cancellation.
#![forbid(unsafe_code)]
use interface as wire;
use mac_abi::{BAD_ABI, BAD_ARGUMENT};
pub use mac_device_interface as interface;
pub use wire::{Description, Mapping};

use crate::{Client, Completion, Error, Phase, Provider, Result};
#[derive(Clone, Copy)]
pub struct DeviceService
{
    provider: Provider
}
impl DeviceService
{
    pub fn new(client: Client) -> Result<Self>
    {
        Ok(Self { provider: client.resolve(wire::CLASS, wire::VERSION)? })
    }

    fn synchronous(self, operation: u32, input: &[u8], output: &mut [u8]) -> Result<usize>
    {
        let mut ticket = 0;
        match self.provider.invoke(&mut ticket, Phase::Start, operation, input, output)?
        {
            Completion::Complete(length) => Ok(length),
            Completion::Pending =>
            {
                // Synchronous operations cannot acquire a deferred lifetime.
                // A mismatched kernel is an ABI error, never fake success.
                let _ = self.provider.invoke(&mut ticket, Phase::Cancel, operation, &[], &mut []);
                Err(Error(BAD_ABI))
            }
        }
    }

    pub fn probe(self, ordinal: u32) -> Result<Description>
    {
        self.describe(wire::PROBE, ordinal)
    }

    /// Inventory slots include empty bus positions; probe each slot up to this
    /// count instead of assuming a board-specific number of devices.
    pub fn count(self) -> Result<u32>
    {
        let mut output = [0; 4];
        if self.synchronous(wire::COUNT, &[], &mut output)? != output.len()
        {
            return Err(Error(BAD_ABI));
        }
        Ok(u32::from_le_bytes(output))
    }

    pub fn claim(self, ordinal: u32) -> Result<Description>
    {
        self.describe(wire::CLAIM, ordinal)
    }

    fn describe(self, operation: u32, ordinal: u32) -> Result<Description>
    {
        let mut output = [0; wire::DESCRIPTION_BYTES];
        let length = self.synchronous(operation, &ordinal.to_le_bytes(), &mut output)?;
        let description = Description::decode(&output[..length]).ok_or(Error(BAD_ABI))?;
        if operation == wire::CLAIM && description.handle == 0
           || operation == wire::PROBE && description.handle != 0
        {
            return Err(Error(BAD_ABI));
        }
        Ok(description)
    }

    pub fn map(self, device: u64, address: u64, length: u64) -> Result<Mapping>
    {
        if length == 0 || address.checked_add(length).is_none()
        {
            return Err(Error(BAD_ARGUMENT));
        }
        let mut input = [0; 24];
        input[..8].copy_from_slice(&device.to_le_bytes());
        input[8..16].copy_from_slice(&address.to_le_bytes());
        input[16..24].copy_from_slice(&length.to_le_bytes());
        let mut output = [0; wire::MAPPING_BYTES];
        let length = self.synchronous(wire::MAP, &input, &mut output)?;
        Mapping::decode(&output[..length]).ok_or(Error(BAD_ABI))
    }

    /// START uses ticket zero. Pending retains that live ticket; POLL/CANCEL use
    /// the same ticket and no retained input buffer. Keep polling cancellation
    /// until terminal acknowledgement before reclaiming hardware-owned storage.
    pub fn reset(self, device: u64, ticket: &mut u64, phase: Phase) -> Result<Completion>
    {
        let input = device.to_le_bytes();
        self.provider.invoke(ticket,
                             phase,
                             wire::RESET,
                             if phase == Phase::Start { &input } else { &[] },
                             &mut [])
    }

    pub fn unmap(self, mapping: u64) -> Result<()>
    {
        self.synchronous(wire::UNMAP, &mapping.to_le_bytes(), &mut [])?;
        Ok(())
    }

    pub fn release(self, device: u64) -> Result<()>
    {
        self.synchronous(wire::RELEASE, &device.to_le_bytes(), &mut [])?;
        Ok(())
    }
}
