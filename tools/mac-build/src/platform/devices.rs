//! Checked platform MMIO inventory shared by device discovery and fatal signaling.
use serde::Deserialize;

use crate::{Result, error};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition
{
    pub id:        String,
    pub bus:       String,
    pub key:       String,
    pub address:   String,
    pub bytes:     u64,
    pub interrupt: u32,
    pub flags:     u32
}
pub struct Checked
{
    pub id:        String,
    pub bus:       [u8; 4],
    pub key:       [u8; 16],
    pub address:   u64,
    pub bytes:     u64,
    pub interrupt: u32,
    pub flags:     u32
}
pub fn check(definitions: &[Definition], memory: &super::memory::Checked) -> Result<Vec<Checked>>
{
    let mut checked: Vec<Checked> = Vec::new();
    for d in definitions
    {
        let bus: [u8; 4] = d.bus
                            .as_bytes()
                            .try_into()
                            .map_err(|_| error("platform MMIO bus must be four ASCII characters"))?;
        if d.id.is_empty() || !bus.iter().all(u8::is_ascii_graphic) || d.key.len() != 32
        {
            return Err(error("platform MMIO device requires an identity, bus and 16-byte \
                              hexadecimal match key"));
        }
        let mut key = [0; 16];
        for (out, hex) in key.iter_mut().zip(d.key.as_bytes().chunks_exact(2))
        {
            let text = core::str::from_utf8(hex).map_err(|_| {
                                                    error("platform MMIO key is not hexadecimal")
                                                })?;
            *out = u8::from_str_radix(text, 16).map_err(|_| {
                                                   error("platform MMIO key is not hexadecimal")
                                               })?;
        }
        let address =
            u64::from_str_radix(d.address
                                 .strip_prefix("0x")
                                 .ok_or_else(|| {
                                     error("platform MMIO address must use 0x hexadecimal")
                                 })?,
                                16).map_err(|_| error("platform MMIO address is invalid"))?;
        let end =
            address.checked_add(d.bytes).ok_or_else(|| error("platform MMIO range overflows"))?;
        if address == 0
           || !address.is_multiple_of(memory.page_bytes)
           || d.bytes == 0
           || !d.bytes.is_multiple_of(memory.page_bytes)
           || !memory.permits_mmio(address..end)
           || d.flags & mac_device_interface::MMIO == 0
           || d.flags
              & !(mac_device_interface::MMIO
                  | mac_device_interface::COHERENT_DMA
                  | mac_device_interface::RESIDENT_OWNED)
              != 0
        {
            return Err(error("platform MMIO range must be page aligned, outside RAM/flash, \
                              with valid device flags"));
        }
        if checked.iter().any(|other| {
                             other.id == d.id
                             || (address < other.address + other.bytes && other.address < end)
                         })
        {
            return Err(error("platform MMIO identities/ranges must be distinct and \
                              non-overlapping"));
        }
        checked.push(Checked { id: d.id.clone(),
                               bus,
                               key,
                               address,
                               bytes: d.bytes,
                               interrupt: d.interrupt,
                               flags: d.flags });
    }
    Ok(checked)
}
pub fn constants(devices: &[Checked]) -> String
{
    let mut result = String::from("\npub const PLATFORM_MMIO_DEVICES: \
                                   &[([u8;4],[u8;16],u64,u64,u32,u32)] = &[\n");
    for d in devices
    {
        result.push_str(&format!("({:?},{:?},{},{},{},{}),\n",
                                 d.bus, d.key, d.address, d.bytes, d.interrupt, d.flags));
    }
    result.push_str("];\n");
    result
}
