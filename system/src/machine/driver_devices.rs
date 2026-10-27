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
const VIRTIO_SLOTS: u32 = super::memory::VIRTIO_MMIO_SLOTS as u32;
pub(crate) const DEVICE_SLOTS: usize =
    VIRTIO_SLOTS as usize + super::power::platform::PLATFORM_MMIO_DEVICES.len();
fn board_device(ordinal: u32) -> Result<Description, Status>
{
    let index = ordinal.checked_sub(VIRTIO_SLOTS).ok_or(NOT_FOUND)? as usize;
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
    if ordinal >= VIRTIO_SLOTS
    {
        return Err(NOT_FOUND);
    }
    Ok((super::memory::VIRTIO_MMIO_BASE + u64::from(ordinal) * super::memory::VIRTIO_MMIO_STRIDE)
       as usize)
}
fn read(base: usize, offset: usize) -> u32
{
    // Only board-described 4 KiB virtio MMIO windows reach this adapter.
    unsafe { u32::from_le(read_volatile((base + offset) as *const u32)) }
}
impl Bus for PlatformBus
{
    fn dma_address(&self, cpu_address: u64, bytes: u64) -> Result<u64, Status>
    {
        super::paging::bus_address(cpu_address, bytes).ok_or(mac_abi::ACCESS_DENIED)
    }

    fn probe(&mut self, ordinal: u32) -> Result<Description, Status>
    {
        if ordinal >= VIRTIO_SLOTS
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
                         interrupt: ordinal + super::memory::VIRTIO_INTERRUPT_BASE as u32,
                         flags: wire::COHERENT_DMA | wire::MMIO | reserved })
    }

    fn request_reset(&mut self, ordinal: u32) -> Result<(), Status>
    {
        if ordinal >= VIRTIO_SLOTS
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
        if ordinal >= VIRTIO_SLOTS
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
    use crate::drivers::devices::lifetime::Registration;
    let kinds = crate::toolbox::handles::registry(|registry| {
                    let [a, b, c, d, e, f] =
                        Registration::ALL.map(|kind| registry.get(kind.specification().kind));
                    Ok::<_, mac_object_table::registry::Error>([a?, b?, c?, d?, e?, f?])
                }).map_err(|error| error.status())?
                  .map_err(|error| crate::toolbox::handles::registry_status(&error))?;
    let devices = Devices::new(PlatformBus, crate::toolbox::handles::system_owner()?, kinds)?;
    DEVICES.publish(devices).map_err(crate::state::AccessError::status)?;
    drivers::register_synchronous_service(Offer { class:          wire::CLASS,
                                                  version:        wire::VERSION,
                                                  request_schema: 1,
                                                  reply_schema:   1,
                                                  max_request:    24,
                                                  max_reply:      wire::DESCRIPTION_BYTES as u32,
                                                  flags:          Offer::KERNEL_ONLY
                                                                  | Offer::EXCLUSIVE,
                                                  capabilities:   0 },
                                          synchronous,
                                          Some(request))?;
    Ok(())
}
fn request(call: ResidentCall<'_>, _: &mut [u8]) -> Result<ResidentCompletion, Status>
{
    let owner = drivers::module_authority(call.caller)?;
    if call.operation == wire::RESET && matches!(call.phase, abi::POLL | abi::CANCEL)
    {
        if !call.input.is_empty()
        {
            return Err(BAD_ARGUMENT);
        }
        return Ok(if with(|devices| devices.reset_pending(&owner, call.ticket))?
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
    if call.operation != wire::RESET
    {
        return Err(NOT_SUPPORTED);
    }
    let device = token(call.input)?;
    Ok(if with(|devices| devices.start_reset(&owner, device, call.ticket))?
       {
           ResidentCompletion::Complete(0)
       }
       else
       {
           ResidentCompletion::Pending
       })
}

/// No request/cancellation storage is created here. All effects and their
/// returned handles complete inside this bounded invocation. A busy lock fails
/// before its operation; retrying cannot duplicate a successful side effect.
pub(crate) fn synchronous(caller: u64, operation: u32, input: &[u8], output: &mut [u8])
                          -> Result<usize, Status>
{
    if !wire::synchronous(operation)
    {
        return Err(NOT_SUPPORTED);
    }
    let owner = drivers::module_authority(caller)?;
    match operation
    {
        wire::COUNT =>
        {
            if !input.is_empty() || output.len() < 4
            {
                return Err(BAD_ARGUMENT);
            }
            output[..4].copy_from_slice(&(DEVICE_SLOTS as u32).to_le_bytes());
            Ok(4)
        }
        wire::PROBE | wire::CLAIM =>
        {
            if output.len() < wire::DESCRIPTION_BYTES
            {
                return Err(BAD_ARGUMENT);
            }
            let ordinal = word(input)?;
            let description = with(|devices| devices.probe(ordinal))?;
            let description = if operation == wire::CLAIM
            {
                drivers::device_allowed(caller, description.bus, description.key)?;
                with(|devices| devices.claim(&owner, ordinal, description))?
            }
            else
            {
                description
            };
            output[..wire::DESCRIPTION_BYTES].copy_from_slice(&description.encode());
            Ok(wire::DESCRIPTION_BYTES)
        }
        wire::MAP =>
        {
            if input.len() != 24 || output.len() < wire::MAPPING_BYTES
            {
                return Err(BAD_ARGUMENT);
            }
            let device = token(&input[..8])?;
            let address = u64::from_le_bytes(input[8..16].try_into().map_err(|_| BAD_ARGUMENT)?);
            let length = u64::from_le_bytes(input[16..24].try_into().map_err(|_| BAD_ARGUMENT)?);
            drivers::owns_memory(caller, address, length)?;
            let mapping = with(|devices| devices.map(&owner, device, address, length))?;
            output[..wire::MAPPING_BYTES].copy_from_slice(&mapping.encode());
            Ok(wire::MAPPING_BYTES)
        }
        wire::UNMAP =>
        {
            let id = token(input)?;
            with(|devices| devices.unmap(&owner, id))?;
            Ok(0)
        }
        wire::RELEASE =>
        {
            let id = token(input)?;
            with(|devices| devices.release(&owner, id))?;
            Ok(0)
        }
        _ => Err(NOT_SUPPORTED)
    }
}

fn token(input: &[u8]) -> Result<u64, Status>
{
    Ok(u64::from_le_bytes(input.try_into().map_err(|_| BAD_ARGUMENT)?))
}

pub(crate) fn cleanup(ticket: mac_object_table::domain::ReleasedResource,
                      kind: crate::drivers::devices::lifetime::Registration)
                      -> Result<(), crate::toolbox::handles::cleanup::Failure>
{
    DEVICES.with_owned(ticket, |ticket, devices| devices.cleanup(ticket, kind))
           .unwrap_or_else(|(error, ticket)| {
               Err(crate::toolbox::handles::cleanup::Failure::devices_unavailable(ticket, error))
           })
}
