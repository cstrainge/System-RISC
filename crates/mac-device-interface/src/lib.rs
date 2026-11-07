//! Resident device-claim and DMA service wire contract for kernel modules.
//! Class-specific payloads stay separate from the class-neutral module ABI.
#![no_std]
#![forbid(unsafe_code)]
pub use mac_module::ClassId;
pub const CLASS: ClassId = ClassId(*b"VDEV");
pub const VERSION: u32 = 1;
pub const PROBE: u32 = 0;
pub const CLAIM: u32 = 1;
pub const MAP: u32 = 2;
pub const RESET: u32 = 3;
pub const UNMAP: u32 = 4;
pub const RELEASE: u32 = 5;
pub const COUNT: u32 = 6;
/// Deferred allocation/initialization; Poll and Cancel retain its real ticket.
pub const ALLOCATE: u32 = 7;
/// Privileged driver asserts device-specific completion before explicit release.
/// Owner cleanup instead requires the Device Manager's actual reset ACK.
pub const FREE_COMPLETED: u32 = 8;
pub const ALLOCATION_BYTES: usize = 32;
pub const DESCRIPTION_BYTES: usize = 52;
pub const MAPPING_BYTES: usize = 24;
pub const COHERENT_DMA: u32 = 1;
pub const MMIO: u32 = 2;
pub const RESIDENT_OWNED: u32 = 4;

/// The magic word a virtio-MMIO transport states first.
pub const VIRTIO_MAGIC: u32 = 0x7472_6976;
/// The transport version this contract drives: the modern register layout.
pub const VIRTIO_VERSION: u32 = 2;
/// How many bytes of identity a transport states: magic, version, device type,
/// then vendor.
pub const VIRTIO_IDENTITY_BYTES: u64 = 16;

/// The device type and vendor a virtio-MMIO window states, or `None` when the
/// window holds no device.
///
/// `register_bytes` is the window's length as the machine stated it, and no read
/// goes past it. An unoccupied transport answers zero rather than not answering,
/// so absence is this returning `None`. The caller must still only pass a window
/// the machine described: reading an address nothing answers on is a fault, and
/// this function cannot tell the two apart.
pub fn virtio_identity(register_bytes: u64, read: impl Fn(usize) -> u32) -> Option<(u32, u32)>
{
    if register_bytes < VIRTIO_IDENTITY_BYTES
       || read(0) != VIRTIO_MAGIC
       || read(4) != VIRTIO_VERSION
    {
        return None;
    }
    let device = read(8);
    if device == 0
    {
        return None;
    }
    Some((device, read(12)))
}

/// Only these operations have a no-deferred-state contract. Reset must retain
/// its explicit request ticket until hardware acknowledges quiescence.
pub const fn synchronous(operation: u32) -> bool
{
    matches!(operation, PROBE | CLAIM | MAP | UNMAP | RELEASE | COUNT | FREE_COMPLETED)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Description
{
    pub handle:         u64,
    pub bus:            ClassId,
    pub key:            [u8; 16],
    pub registers:      u64,
    pub register_bytes: u64,
    pub interrupt:      u32,
    pub flags:          u32
}
impl Description
{
    pub fn encode(self) -> [u8; DESCRIPTION_BYTES]
    {
        let mut out = [0; DESCRIPTION_BYTES];
        out[..8].copy_from_slice(&self.handle.to_le_bytes());
        out[8..12].copy_from_slice(&self.bus.0);
        out[12..28].copy_from_slice(&self.key);
        out[28..36].copy_from_slice(&self.registers.to_le_bytes());
        out[36..44].copy_from_slice(&self.register_bytes.to_le_bytes());
        out[44..48].copy_from_slice(&self.interrupt.to_le_bytes());
        out[48..52].copy_from_slice(&self.flags.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self>
    {
        if bytes.len() != DESCRIPTION_BYTES
        {
            return None;
        }
        let description =
            Self { handle:         u64::from_le_bytes(bytes[..8].try_into().ok()?),
                   bus:            ClassId(bytes[8..12].try_into().ok()?),
                   key:            bytes[12..28].try_into().ok()?,
                   registers:      u64::from_le_bytes(bytes[28..36].try_into().ok()?),
                   register_bytes: u64::from_le_bytes(bytes[36..44].try_into().ok()?),
                   interrupt:      u32::from_le_bytes(bytes[44..48].try_into().ok()?),
                   flags:          u32::from_le_bytes(bytes[48..52].try_into().ok()?) };
        if (description.handle != 0 && description.handle <= u32::MAX as u64)
           || !description.bus.0.iter().all(u8::is_ascii_graphic)
           || description.register_bytes == 0
           || description.registers.checked_add(description.register_bytes).is_none()
           || description.flags & !(COHERENT_DMA | MMIO | RESIDENT_OWNED) != 0
        {
            return None;
        }
        Some(description)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mapping
{
    pub handle:  u64,
    /// Device-visible bus address; never a CPU pointer, even for coherent DMA.
    pub address: u64,
    pub length:  u64
}
impl Mapping
{
    pub fn encode(self) -> [u8; MAPPING_BYTES]
    {
        let mut out = [0; MAPPING_BYTES];
        out[..8].copy_from_slice(&self.handle.to_le_bytes());
        out[8..16].copy_from_slice(&self.address.to_le_bytes());
        out[16..24].copy_from_slice(&self.length.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self>
    {
        if bytes.len() != MAPPING_BYTES
        {
            return None;
        }
        let mapping = Self { handle:  u64::from_le_bytes(bytes[..8].try_into().ok()?),
                             address: u64::from_le_bytes(bytes[8..16].try_into().ok()?),
                             length:  u64::from_le_bytes(bytes[16..24].try_into().ok()?) };
        if mapping.handle <= u32::MAX as u64
           || mapping.length == 0
           || mapping.address.checked_add(mapping.length).is_none()
        {
            return None;
        }
        Some(mapping)
    }
}

/// Owned DMA allocation. CPU and device addresses are distinct; neither an
/// address nor a copied handle grants another module ownership.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Allocation
{
    pub handle:         u64,
    pub cpu_address:    u64,
    pub device_address: u64,
    pub length:         u64
}
impl Allocation
{
    pub fn encode(self) -> [u8; ALLOCATION_BYTES]
    {
        let mut out = [0; ALLOCATION_BYTES];
        for (slot, value) in
            out.chunks_exact_mut(8)
               .zip([self.handle, self.cpu_address, self.device_address, self.length])
        {
            slot.copy_from_slice(&value.to_le_bytes());
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self>
    {
        if bytes.len() != ALLOCATION_BYTES
        {
            return None;
        }
        let value = Self { handle:         u64::from_le_bytes(bytes[0..8].try_into().ok()?),
                           cpu_address:    u64::from_le_bytes(bytes[8..16].try_into().ok()?),
                           device_address: u64::from_le_bytes(bytes[16..24].try_into().ok()?),
                           length:         u64::from_le_bytes(bytes[24..32].try_into().ok()?) };
        (value.handle > u32::MAX as u64
         && value.cpu_address != 0
         && value.length != 0
         && value.cpu_address.checked_add(value.length).is_some()
         && value.device_address.checked_add(value.length).is_some()).then_some(value)
    }
}
