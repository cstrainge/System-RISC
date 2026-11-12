//! Thin checked access to the resident device/DMA service. Claims and mappings
//! remain pinned in System until explicit reset, unmap and release succeed.
//! Hardware adapters retain their DMA storage throughout pending cancellation.
#![deny(unsafe_code)]
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
        self.provider.synchronous(operation, input, output)
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

    /// The firmware power conduit the machine's tree declares, as
    /// `mac_abi::boot::conduit` encodes it.
    ///
    /// A function the machine declares has no register window, so there is no
    /// device to match or claim: the resident reports what the boot read out of
    /// the tree, and the caller decides whether it implements that conduit.
    pub fn power_conduit(self) -> Result<u64>
    {
        let mut output = [0; 8];
        if self.synchronous(wire::CONDUIT, &[], &mut output)? != output.len()
        {
            return Err(Error(BAD_ABI));
        }
        Ok(u64::from_le_bytes(output))
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

    /// Translate pinned module CPU storage for the claimed device. The returned
    /// Mapping.address is a bus address and must never be dereferenced by Rust.
    pub fn map(self, device: u64, address: u64, length: u64) -> Result<Mapping>
    {
        self.map_with(device, address, length, 0)
    }

    /// As [`Self::map`], by a driver that brings the mapping's two views into
    /// step itself with [`Self::sync_for_device`] and [`Self::sync_for_cpu`].
    /// A device outside the CPU's caches is mapped only this way. On a
    /// coherent device the calls do nothing, so the same driver is correct on
    /// both.
    pub fn map_synchronized(self, device: u64, address: u64, length: u64) -> Result<Mapping>
    {
        self.map_with(device, address, length, wire::MAP_SYNCHRONIZED)
    }

    fn map_with(self, device: u64, address: u64, length: u64, flags: u64) -> Result<Mapping>
    {
        if length == 0 || address.checked_add(length).is_none()
        {
            return Err(Error(BAD_ARGUMENT));
        }
        let mut input = [0; 32];
        input[..8].copy_from_slice(&device.to_le_bytes());
        input[8..16].copy_from_slice(&address.to_le_bytes());
        input[16..24].copy_from_slice(&length.to_le_bytes());
        input[24..].copy_from_slice(&flags.to_le_bytes());
        let mut output = [0; wire::MAPPING_BYTES];
        let length = self.synchronous(wire::MAP, &input, &mut output)?;
        Mapping::decode(&output[..length]).ok_or(Error(BAD_ABI))
    }

    fn sync(self, handle: u64, offset: u64, length: u64, direction: u64) -> Result<()>
    {
        let mut input = [0; 32];
        input[..8].copy_from_slice(&handle.to_le_bytes());
        input[8..16].copy_from_slice(&offset.to_le_bytes());
        input[16..24].copy_from_slice(&length.to_le_bytes());
        input[24..].copy_from_slice(&direction.to_le_bytes());
        self.synchronous(wire::SYNC, &input, &mut [])?;
        Ok(())
    }

    /// The CPU has written `length` bytes at `offset` of a mapping, view or
    /// allocation and the device is about to read them - or is about to write
    /// there. Call it before the device is told to go.
    pub fn sync_for_device(self, handle: u64, offset: u64, length: u64) -> Result<()>
    {
        self.sync(handle, offset, length, wire::SYNC_FOR_DEVICE)
    }

    /// The device has written `length` bytes at `offset` and the CPU is about
    /// to read them.
    pub fn sync_for_cpu(self, handle: u64, offset: u64, length: u64) -> Result<()>
    {
        self.sync(handle, offset, length, wire::SYNC_FOR_CPU)
    }

    /// A CPU view of `length` bytes of memory at physical address `physical`
    /// that the machine's firmware owns and shares with this device. Refused
    /// for anything the platform does not declare as the firmware's. Give it
    /// back with [`Self::unmap`].
    pub fn shared(self, device: u64, physical: u64, length: u64) -> Result<wire::View>
    {
        let mut input = [0; 24];
        input[..8].copy_from_slice(&device.to_le_bytes());
        input[8..16].copy_from_slice(&physical.to_le_bytes());
        input[16..].copy_from_slice(&length.to_le_bytes());
        let mut output = [0; wire::MAPPING_BYTES];
        let length = self.synchronous(wire::SHARED, &input, &mut output)?;
        wire::View::decode(&output[..length]).ok_or(Error(BAD_ABI))
    }

    /// Allocate pinned module-owned DMA storage through the real request
    /// ticket. Initialization advances in bounded resident turns. A Pending or
    /// pre-dispatch refusal retains a nonzero ticket for Poll/Cancel. Terminal
    /// completion clears it. A completed allocation may win cancellation:
    /// Cancel can return Some(buffer), which the caller must retain and release.
    pub fn allocate(self, device: u64, bytes: u64, ticket: &mut u64, phase: Phase)
                    -> Result<(Completion, Option<wire::Allocation>)>
    {
        self.allocate_with(device, bytes, 0, ticket, phase)
    }

    /// As [`Self::allocate`], for storage the device itself reads or writes: a
    /// message it is handed the address of, a descriptor ring, a transfer
    /// buffer. On a device outside the CPU's caches System returns memory no
    /// cache holds, so the driver performs no cache maintenance on it. Keep
    /// such buffers small.
    pub fn allocate_device_accessed(self, device: u64, bytes: u64, ticket: &mut u64, phase: Phase)
                                    -> Result<(Completion, Option<wire::Allocation>)>
    {
        self.allocate_with(device, bytes, wire::ALLOCATE_DEVICE_ACCESSED, ticket, phase)
    }

    fn allocate_with(self, device: u64, bytes: u64, flags: u64, ticket: &mut u64, phase: Phase)
                     -> Result<(Completion, Option<wire::Allocation>)>
    {
        let mut input = [0; 24];
        input[..8].copy_from_slice(&device.to_le_bytes());
        input[8..16].copy_from_slice(&bytes.to_le_bytes());
        input[16..].copy_from_slice(&flags.to_le_bytes());
        let mut output = [0; wire::ALLOCATION_BYTES];
        let completion = self.provider.invoke(ticket,
                                               phase,
                                               wire::ALLOCATE,
                                               if phase == Phase::Start { &input } else { &[] },
                                               &mut output)?;
        let buffer = match completion
        {
            Completion::Complete(0) if phase == Phase::Cancel => None,
            Completion::Complete(wire::ALLOCATION_BYTES) =>
            {
                Some(wire::Allocation::decode(&output).ok_or(Error(BAD_ABI))?)
            }
            Completion::Pending => None,
            _ => return Err(Error(BAD_ABI))
        };
        Ok((completion, buffer))
    }

    /// Release a completed hardware allocation. The resident also refuses while
    /// System holds any CPU surface pins. Refusal retains the original handle.
    ///
    /// # Safety
    /// The driver must have actual device-specific acknowledgment that no
    /// outstanding or future DMA can access this allocation (for example a
    /// completed resource detach/unref), or an acknowledged full device reset.
    /// All module CPU borrows/access must have ended as well. The driver owns
    /// synchronization of its workers; raw addresses are not CPU reader leases.
    /// Elapsed time and submitting a command do not establish completion.
    #[allow(unsafe_code)]
    pub unsafe fn free_completed(self, allocation: &mut wire::Allocation) -> Result<()>
    {
        self.synchronous(wire::FREE_COMPLETED, &allocation.handle.to_le_bytes(), &mut [])?;
        allocation.handle = 0;
        Ok(())
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
