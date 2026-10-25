//! QEMU virt's MMIO device bus. Hardware reads/reset are the only unsafe
//! operations; authorization, claim generations and DMA lifetimes remain safe.
use core::ptr::{read_volatile, write_volatile};

use mac_abi::{BAD_ARGUMENT, NOT_FOUND, NOT_SUPPORTED, Status};
use mac_device_interface::{self as wire, Description};
use mac_module::{ClassId, Offer, abi};

use crate::{drivers::{self, ResidentCall, ResidentCompletion,
                      devices::{Bus, Devices}},
            state::TryState};
struct PlatformBus;
pub(crate) const DEVICE_SLOTS: usize = 8 + super::power::platform::PLATFORM_MMIO_DEVICES.len();
fn board_device(ordinal: u32) -> Result<Description, Status>
{
    let index = ordinal.checked_sub(8).ok_or(NOT_FOUND)? as usize;
    let &(bus, key, registers, register_bytes, interrupt, flags) =
        super::power::platform::PLATFORM_MMIO_DEVICES.get(index).ok_or(NOT_FOUND)?;
    Ok(Description { handle: 0,
                     bus: ClassId(bus),
                     key,
                     registers,
                     register_bytes,
                     interrupt,
                     flags })
}
fn base(ordinal: u32) -> Result<usize, Status>
{
    if ordinal >= 8
    {
        return Err(NOT_FOUND);
    }
    Ok(0x1000_1000 + ordinal as usize * 4096)
}
fn read(base: usize, offset: usize) -> u32
{
    // Only board-described 4 KiB virtio MMIO windows reach this adapter.
    unsafe { u32::from_le(read_volatile((base + offset) as *const u32)) }
}
impl Bus for PlatformBus
{
    fn probe(&mut self, ordinal: u32) -> Result<Description, Status>
    {
        if ordinal >= 8
        {
            return board_device(ordinal);
        }
        let address = base(ordinal)?;
        if read(address, 0) != 0x7472_6976 || read(address, 4) != 2
        {
            return Err(NOT_FOUND);
        }
        let device = read(address, 8);
        if device == 0
        {
            return Err(NOT_FOUND);
        }
        let mut key = [0; 16];
        key[..4].copy_from_slice(&device.to_le_bytes());
        key[4..8].copy_from_slice(&read(address, 12).to_le_bytes());
        // Firmware still owns block I/O, and System still owns input. Their
        // extraction is a separate milestone; a module cannot reset them here.
        let reserved = if matches!(device, 2 | 18) { wire::RESIDENT_OWNED } else { 0 };
        Ok(Description { handle: 0,
                         bus: ClassId(*b"VMMI"),
                         key,
                         registers: address as u64,
                         register_bytes: 4096,
                         interrupt: ordinal + 1,
                         flags: wire::COHERENT_DMA | wire::MMIO | reserved })
    }

    fn request_reset(&mut self, ordinal: u32) -> Result<(), Status>
    {
        if ordinal >= 8
        {
            return if board_device(ordinal)?.flags & wire::COHERENT_DMA == 0
            {
                Ok(())
            }
            else
            {
                Err(NOT_SUPPORTED)
            };
        }
        let address = base(ordinal)?;
        unsafe {
            write_volatile((address + 0x70) as *mut u32, 0);
            core::arch::asm!("fence iorw, iorw", options(nostack));
        }
        Ok(())
    }

    fn stopped(&mut self, ordinal: u32) -> Result<bool, Status>
    {
        if ordinal >= 8
        {
            return if board_device(ordinal)?.flags & wire::COHERENT_DMA == 0
            {
                Ok(true)
            }
            else
            {
                Err(NOT_SUPPORTED)
            };
        }
        Ok(read(base(ordinal)?, 0x70) == 0)
    }
}
static DEVICES: TryState<Devices<PlatformBus>> = TryState::new();
/// Boot discovery takes one bounded snapshot. Match policy stays in the generic
/// loader; only this adapter knows the board's MMIO slots.
pub(crate) fn inventory() -> Result<[Option<Description>; DEVICE_SLOTS], Status>
{
    with(|devices| {
        let mut result = [None; DEVICE_SLOTS];
        for (ordinal, slot) in result.iter_mut().enumerate()
        {
            match devices.probe(ordinal as u32)
            {
                Ok(description) => *slot = Some(description),
                Err(NOT_FOUND) => (),
                Err(error) => return Err(error)
            }
        }
        Ok(result)
    })
}
fn with<R>(call: impl FnOnce(&mut Devices<PlatformBus>) -> Result<R, Status>) -> Result<R, Status>
{
    DEVICES.with(call).map_err(crate::state::AccessError::status)?
}
fn word(input: &[u8]) -> Result<u32, Status>
{
    Ok(u32::from_le_bytes(input.try_into().map_err(|_| BAD_ARGUMENT)?))
}
pub(crate) fn initialize() -> Result<(), Status>
{
    DEVICES.publish(Devices::new(PlatformBus)).map_err(crate::state::AccessError::status)?;
    drivers::register_service(Offer { class:          wire::CLASS,
                                      version:        wire::VERSION,
                                      request_schema: 1,
                                      reply_schema:   1,
                                      max_request:    24,
                                      max_reply:      48,
                                      flags:          Offer::KERNEL_ONLY | Offer::EXCLUSIVE,
                                      capabilities:   0 },
                              request)?;
    Ok(())
}
fn request(call: ResidentCall<'_>, output: &mut [u8]) -> Result<ResidentCompletion, Status>
{
    if call.operation == wire::RESET && matches!(call.phase, abi::POLL | abi::CANCEL)
    {
        if !call.input.is_empty()
        {
            return Err(BAD_ARGUMENT);
        }
        return Ok(if with(|devices| devices.reset_pending(call.caller, call.ticket))?
                  {
                      ResidentCompletion::Complete(0)
                  }
                  else
                  {
                      ResidentCompletion::Pending
                  });
    }
    if call.phase != abi::START
    {
        return Err(BAD_ARGUMENT);
    }
    match call.operation
    {
        wire::COUNT =>
        {
            if !call.input.is_empty() || output.len() < 4
            {
                return Err(BAD_ARGUMENT);
            }
            output[..4].copy_from_slice(&(DEVICE_SLOTS as u32).to_le_bytes());
            Ok(ResidentCompletion::Complete(4))
        }
        wire::PROBE | wire::CLAIM =>
        {
            if output.len() < wire::DESCRIPTION_BYTES
            {
                return Err(BAD_ARGUMENT);
            }
            let ordinal = word(call.input)?;
            let description = with(|devices| devices.probe(ordinal))?;
            let description = if call.operation == wire::CLAIM
            {
                drivers::device_allowed(call.caller, description.bus, description.key)?;
                with(|devices| devices.claim(call.caller, ordinal, description))?
            }
            else
            {
                description
            };
            output[..wire::DESCRIPTION_BYTES].copy_from_slice(&description.encode());
            Ok(ResidentCompletion::Complete(wire::DESCRIPTION_BYTES))
        }
        wire::MAP =>
        {
            if call.input.len() != 24
               || call.input[4..8] != [0; 4]
               || output.len() < wire::MAPPING_BYTES
            {
                return Err(BAD_ARGUMENT);
            }
            let device = word(&call.input[..4])?;
            let address =
                u64::from_le_bytes(call.input[8..16].try_into().map_err(|_| BAD_ARGUMENT)?);
            let length =
                u64::from_le_bytes(call.input[16..24].try_into().map_err(|_| BAD_ARGUMENT)?);
            drivers::owns_memory(call.caller, address, length)?;
            let mapping = with(|devices| devices.map(call.caller, device, address, length))?;
            output[..wire::MAPPING_BYTES].copy_from_slice(&mapping.encode());
            Ok(ResidentCompletion::Complete(wire::MAPPING_BYTES))
        }
        wire::RESET =>
        {
            let device = word(call.input)?;
            Ok(if with(|devices| devices.reset(call.caller, device, call.ticket, true))?
               {
                   ResidentCompletion::Complete(0)
               }
               else
               {
                   ResidentCompletion::Pending
               })
        }
        wire::UNMAP =>
        {
            let id = word(call.input)?;
            with(|devices| devices.unmap(call.caller, id))?;
            Ok(ResidentCompletion::Complete(0))
        }
        wire::RELEASE =>
        {
            let id = word(call.input)?;
            with(|devices| devices.release(call.caller, id))?;
            Ok(ResidentCompletion::Complete(0))
        }
        _ => Err(NOT_SUPPORTED)
    }
}
