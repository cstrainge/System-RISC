//! QEMU virt's MMIO device bus. Hardware reads/reset are the only unsafe
//! operations; authorization, claim generations and DMA lifetimes remain safe.
use core::ptr::{read_volatile, write_volatile};

use mac_abi::{BAD_ARGUMENT, NO_MEMORY, NOT_FOUND, NOT_SUPPORTED, Status};
use mac_device_interface::{self as wire, Description};
use mac_module::{ClassId, Offer, abi};

use crate::{drivers::{self, ResidentCall, ResidentCompletion,
                      devices::{Bus, Devices}},
            machine::platform_devices::{self, Transport},
            state::TryState};
struct PlatformBus;
/// The virtio device type of a block device, and of an input device. Both stay
/// resident-owned until their explicit extraction milestones.
const VIRTIO_BLOCK: u32 = 2;
const VIRTIO_INPUT: u32 = 18;
/// The transport's status register, which states whether it is stopped.
const VIRTIO_STATUS: usize = 0x70;

/// One ordinal on this bus: a virtio-MMIO transport the machine places, or a
/// peripheral it describes some other way.
///
/// The machine's tree decides the split. Nothing here is a compiled-in device
/// list, and no ordinal is turned into an address.
enum Slot
{
    Transport(Transport),
    Board(usize)
}

fn slot(ordinal: u32) -> Result<Slot, Status>
{
    let index = ordinal as usize;
    if let Some(transport) = platform_devices::transport(index)
    {
        return Ok(Slot::Transport(transport));
    }
    let board = index.checked_sub(platform_devices::transport_count()).ok_or(NOT_FOUND)?;
    Ok(Slot::Board(board))
}

/// How many ordinals this bus answers for: the machine's transports, then the
/// other peripherals it describes.
fn device_count() -> usize
{
    platform_devices::transport_count() + platform_devices::count()
}

fn board_device(index: usize) -> Result<Description, Status>
{
    let device = platform_devices::get(index).ok_or(NOT_FOUND)?;
    Ok(Description { handle: 0,
                     bus: ClassId(device.bus),
                     key: device.key,
                     registers: device.registers,
                     register_bytes: device.register_bytes,
                     interrupt: device.interrupt,
                     flags: device.flags })
}
fn read(base: usize, offset: usize) -> u32
{
    // Only windows the machine's tree describes reach this adapter, so a read
    // here is always inside a register block the machine said exists.
    unsafe { u32::from_le(read_volatile((base + offset) as *const u32)) }
}

/// What a transport window holds, decided from the window's own registers.
///
/// The machine states that the window exists, so reading it is a lookup rather
/// than a probe of an address that may not answer: an unoccupied transport
/// answers zero, and `None` is the absence. Nothing here derives a window from
/// an ordinal, and an address the machine did not describe is never touched.
fn describe(transport: Transport, read: impl Fn(usize) -> u32) -> Option<Description>
{
    let (device, vendor) = wire::virtio_identity(transport.register_bytes, read)?;
    let mut key = [0; 16];
    key[..4].copy_from_slice(&device.to_le_bytes());
    key[4..8].copy_from_slice(&vendor.to_le_bytes());
    // Firmware still owns block I/O, and System still owns input. Their
    // extraction is a separate milestone; a module cannot reset them here.
    let reserved =
        if matches!(device, VIRTIO_BLOCK | VIRTIO_INPUT) { wire::RESIDENT_OWNED } else { 0 };
    // Placement comes from the tree. `COHERENT_DMA` does not: it is this
    // adapter's statement about the virtio-MMIO transport the resident input
    // driver already performs DMA through, and a machine whose tree is silent
    // about `dma-coherent` (QEMU's RISC-V virt) would otherwise be refused the
    // DMA it actually performs.
    Some(Description { handle: 0,
                       bus: ClassId(*b"VMMI"),
                       key,
                       registers: transport.registers,
                       register_bytes: transport.register_bytes,
                       interrupt: transport.interrupt,
                       flags: wire::COHERENT_DMA | wire::MMIO | reserved })
}

fn transport_address(transport: Transport) -> Result<usize, Status>
{
    usize::try_from(transport.registers).map_err(|_| NOT_SUPPORTED)
}

impl Bus for PlatformBus
{
    fn dma_address(&self, cpu_address: u64, bytes: u64) -> Result<u64, Status>
    {
        crate::kernel::module_dma_address(cpu_address, bytes)
    }

    fn device_address(&self, ordinal: u32, physical: u64, bytes: u64) -> Result<u64, Status>
    {
        match slot(ordinal)?
        {
            // A virtio-MMIO transport sits at the root and sees memory as the
            // CPU does.
            Slot::Transport(_) => Ok(physical),
            Slot::Board(index) =>
            {
                platform_devices::get(index).ok_or(NOT_FOUND)?
                                            .dma
                                            .bus_address(physical, bytes)
                                            .ok_or(NOT_SUPPORTED)
            }
        }
    }

    fn synchronize(&self, cpu_address: u64, bytes: u64, for_device: bool) -> Result<(), Status>
    {
        let address = usize::try_from(cpu_address).map_err(|_| BAD_ARGUMENT)?;
        let bytes = usize::try_from(bytes).map_err(|_| BAD_ARGUMENT)?;
        // SAFETY: the device manager passes only a span it recorded for a live
        // mapping of the calling module, which is mapped in the space this
        // synchronous call runs under. Discarding lines is the caller's stated
        // intent: the device has written the span.
        unsafe {
            if for_device
            {
                crate::machine::cache::clean(address, bytes);
            }
            else
            {
                crate::machine::cache::invalidate(address, bytes);
            }
        }
        Ok(())
    }

    fn shared_view(&self, ordinal: u32, physical: u64, bytes: u64) -> Result<u64, Status>
    {
        match slot(ordinal)?
        {
            Slot::Transport(_) => Err(NOT_SUPPORTED),
            Slot::Board(_) =>
            {
                crate::machine::paging::firmware_memory_view(physical, bytes).ok_or(NOT_SUPPORTED)
            }
        }
    }

    fn probe(&mut self, ordinal: u32) -> Result<Description, Status>
    {
        match slot(ordinal)?
        {
            Slot::Transport(transport) =>
            {
                let address = transport_address(transport)?;
                describe(transport, |offset| read(address, offset)).ok_or(NOT_FOUND)
            }
            Slot::Board(index) => board_device(index)
        }
    }

    fn request_reset(&mut self, ordinal: u32) -> Result<(), Status>
    {
        match slot(ordinal)?
        {
            Slot::Transport(transport) =>
            {
                let address = transport_address(transport)?;
                unsafe {
                    write_volatile((address + VIRTIO_STATUS) as *mut u32, 0);
                    crate::machine::cpu::barrier();
                }
                Ok(())
            }
            Slot::Board(index) =>
            {
                if board_device(index)?.flags & wire::COHERENT_DMA == 0
                {
                    Ok(())
                }
                else
                {
                    Err(NOT_SUPPORTED)
                }
            }
        }
    }

    fn stopped(&mut self, ordinal: u32) -> Result<bool, Status>
    {
        match slot(ordinal)?
        {
            Slot::Transport(transport) =>
            {
                let address = transport_address(transport)?;
                Ok(read(address, VIRTIO_STATUS) == 0)
            }
            Slot::Board(index) =>
            {
                if board_device(index)?.flags & wire::COHERENT_DMA == 0
                {
                    Ok(true)
                }
                else
                {
                    Err(NOT_SUPPORTED)
                }
            }
        }
    }
}
static DEVICES: TryState<Devices<PlatformBus>> = TryState::new();
/// Boot discovery takes one bounded snapshot. Match policy stays in the generic
/// loader; only this adapter knows how to read the machine's transports.
///
/// The snapshot is as long as the machine is: an unoccupied transport is a
/// `None` at its own ordinal, so absence is a lookup result and never a probe of
/// an address the machine did not describe.
pub(crate) fn inventory() -> Result<alloc::vec::Vec<Option<Description>>, Status>
{
    with(|devices| {
        let count = device_count();
        let mut result = alloc::vec::Vec::new();
        result.try_reserve_exact(count).map_err(|_| NO_MEMORY)?;
        for ordinal in 0..count
        {
            let ordinal = u32::try_from(ordinal).map_err(|_| NOT_SUPPORTED)?;
            match devices.probe(ordinal)
            {
                Ok(description) => result.push(Some(description)),
                Err(NOT_FOUND) => result.push(None),
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
                                                  max_request:    32,
                                                  max_reply:      wire::DESCRIPTION_BYTES as u32,
                                                  flags:          Offer::KERNEL_ONLY
                                                                  | Offer::EXCLUSIVE,
                                                  capabilities:   0 },
                                          synchronous,
                                          Some(request))?;
    Ok(())
}
fn request(call: ResidentCall<'_>, output: &mut [u8]) -> Result<ResidentCompletion, Status>
{
    let owner = drivers::module_authority(call.caller)?;
    if call.operation == wire::ALLOCATE
    {
        if call.phase == abi::START
        {
            if !matches!(call.input.len(), 16 | 24) || output.len() < wire::ALLOCATION_BYTES
            {
                return Err(BAD_ARGUMENT);
            }
            let device = token(&call.input[..8])?;
            let bytes = token(&call.input[8..16])?;
            // The flags word is optional; a request without it asks for none.
            // A flag this System does not know is refused, not ignored: the
            // driver asked for a property of the memory it will not get.
            let flags = match call.input.get(16..24)
            {
                Some(word) => u64::from_le_bytes(word.try_into().map_err(|_| BAD_ARGUMENT)?),
                None => 0
            };
            if flags & !wire::ALLOCATE_DEVICE_ACCESSED != 0
            {
                return Err(BAD_ARGUMENT);
            }
            let device_accessed = flags & wire::ALLOCATE_DEVICE_ACCESSED != 0;
            let account = drivers::module_allocation(call.caller)?;
            crate::allocation::with_kernel_allocation(account, || {
                with(|devices| {
                    devices.start_allocate(&owner, device, bytes, call.ticket, device_accessed)
                })
            })??;
        }
        else if !matches!(call.phase, abi::POLL | abi::CANCEL) || !call.input.is_empty()
        {
            return Err(BAD_ARGUMENT);
        }
        if call.phase != abi::CANCEL && output.len() < wire::ALLOCATION_BYTES
        {
            return Err(BAD_ARGUMENT);
        }
        let result = with(|devices| {
            devices.advance_allocate(&owner, call.ticket, call.phase == abi::CANCEL)
        })?;
        if call.phase == abi::CANCEL
        {
            return Ok(ResidentCompletion::Complete(0));
        }
        return Ok(match result
        {
            Some(buffer) =>
            {
                output[..wire::ALLOCATION_BYTES].copy_from_slice(&buffer.encode());
                ResidentCompletion::Complete(wire::ALLOCATION_BYTES)
            }
            None => ResidentCompletion::Pending
        });
    }
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
        wire::CONDUIT =>
        {
            if !input.is_empty() || output.len() < 8
            {
                return Err(BAD_ARGUMENT);
            }
            output[..8].copy_from_slice(&platform_devices::power_conduit().to_le_bytes());
            Ok(8)
        }
        wire::COUNT =>
        {
            if !input.is_empty() || output.len() < 4
            {
                return Err(BAD_ARGUMENT);
            }
            let count = u32::try_from(device_count()).map_err(|_| NOT_SUPPORTED)?;
            output[..4].copy_from_slice(&count.to_le_bytes());
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
                // The retained claim belongs to this module even when the
                // device is first opened by an application-owned request.
                let frame = drivers::module_frame(caller)?;
                let account = drivers::module_allocation(caller)?;
                crate::allocation::with_kernel_allocation(account, || {
                    with(|devices| devices.claim(&owner, ordinal, description, frame))
                })??
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
            if !matches!(input.len(), 24 | 32) || output.len() < wire::MAPPING_BYTES
            {
                return Err(BAD_ARGUMENT);
            }
            let device = token(&input[..8])?;
            let address = u64::from_le_bytes(input[8..16].try_into().map_err(|_| BAD_ARGUMENT)?);
            let length = u64::from_le_bytes(input[16..24].try_into().map_err(|_| BAD_ARGUMENT)?);
            let flags = match input.get(24..32)
            {
                Some(word) => u64::from_le_bytes(word.try_into().map_err(|_| BAD_ARGUMENT)?),
                None => 0
            };
            if flags & !wire::MAP_SYNCHRONIZED != 0
            {
                return Err(BAD_ARGUMENT);
            }
            let synchronized = flags & wire::MAP_SYNCHRONIZED != 0;
            drivers::owns_memory(caller, address, length)?;
            // DMA metadata outlives the originating I/O operation and follows
            // the same genuine module ownership as its admitted CPU storage.
            let account = drivers::module_allocation(caller)?;
            let mapping = crate::allocation::with_kernel_allocation(account, || {
                with(|devices| devices.map(&owner, device, address, length, synchronized))
            })??;
            output[..wire::MAPPING_BYTES].copy_from_slice(&mapping.encode());
            Ok(wire::MAPPING_BYTES)
        }
        wire::SYNC =>
        {
            if input.len() != 32 || !output.is_empty()
            {
                return Err(BAD_ARGUMENT);
            }
            let word = |at: usize| -> Result<u64, Status> {
                Ok(u64::from_le_bytes(input[at..at + 8].try_into().map_err(|_| BAD_ARGUMENT)?))
            };
            let mapping = token(&input[..8])?;
            let for_device = match word(24)?
            {
                wire::SYNC_FOR_DEVICE => true,
                wire::SYNC_FOR_CPU => false,
                _ => return Err(BAD_ARGUMENT)
            };
            with(|devices| devices.sync(&owner, mapping, word(8)?, word(16)?, for_device))?;
            Ok(0)
        }
        wire::INTERRUPT =>
        {
            if input.len() != 16 || output.len() < 4
            {
                return Err(BAD_ARGUMENT);
            }
            let device = token(&input[..8])?;
            let action = u64::from_le_bytes(input[8..16].try_into().map_err(|_| BAD_ARGUMENT)?);
            let line = with(|devices| devices.interrupt_line(&owner, device))?;
            let result = match action
            {
                wire::INTERRUPT_TAKE => super::device_interrupts::take_for_module(line, caller),
                wire::INTERRUPT_RELEASE => super::device_interrupts::release(line, caller),
                _ => return Err(BAD_ARGUMENT)
            };
            if let Err(status) = result
            {
                // A driver that is refused carries on without its line, so the
                // refusal is said here or it is said nowhere.
                serial!("DEVICE_INTERRUPT_NOT_TAKEN line={line} module={caller} action={action} \
                         status={status}");
                return Err(status);
            }
            output[..4].copy_from_slice(&line.to_le_bytes());
            Ok(4)
        }
        wire::SHARED =>
        {
            if input.len() != 24 || output.len() < wire::MAPPING_BYTES
            {
                return Err(BAD_ARGUMENT);
            }
            let device = token(&input[..8])?;
            let address = u64::from_le_bytes(input[8..16].try_into().map_err(|_| BAD_ARGUMENT)?);
            let length = u64::from_le_bytes(input[16..24].try_into().map_err(|_| BAD_ARGUMENT)?);
            let account = drivers::module_allocation(caller)?;
            let view = crate::allocation::with_kernel_allocation(account, || {
                with(|devices| devices.shared(&owner, device, address, length))
            })??;
            output[..wire::MAPPING_BYTES].copy_from_slice(&view.encode());
            Ok(wire::MAPPING_BYTES)
        }
        wire::FREE_COMPLETED =>
        {
            let id = token(input)?;
            with(|devices| devices.free_completed(&owner, id))?;
            Ok(0)
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
            let line = with(|devices| devices.interrupt_line(&owner, id)).ok();
            with(|devices| devices.release(&owner, id))?;
            if let Some(line) = line
            {
                super::device_interrupts::release_line(line);
            }
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

/// System display publication authenticates the provider's real module and pins
/// the exact registered allocation, never an arbitrary reported CPU address.
pub(crate) fn pin_buffer(module: u64, id: u64)
                         -> Result<alloc::sync::Arc<crate::machine::dma::Buffer>, Status>
{
    let owner = drivers::module_authority(module)?;
    with(|devices| devices.pin_buffer(&owner, id))
}
