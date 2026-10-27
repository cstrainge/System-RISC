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
pub const DESCRIPTION_BYTES: usize = 52;
pub const MAPPING_BYTES: usize = 24;
pub const COHERENT_DMA: u32 = 1;
pub const MMIO: u32 = 2;
pub const RESIDENT_OWNED: u32 = 4;

/// Only these operations have a no-deferred-state contract. Reset must retain
/// its explicit request ticket until hardware acknowledges quiescence.
pub const fn synchronous(operation: u32) -> bool
{
    matches!(operation, PROBE | CLAIM | MAP | UNMAP | RELEASE | COUNT)
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
