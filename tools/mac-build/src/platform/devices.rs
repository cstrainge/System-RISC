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
    /// The ARM interrupt this window raises.
    ///
    /// Three states, and they are deliberately three: a number is an interrupt,
    /// JSON `null` is a window that raises none, and a field that is absent is
    /// refused. A plain optional field would collapse the last two, so a typo
    /// that dropped the key would read as "this block has no interrupt" and the
    /// record would quietly lose a fact. A device with no interrupt has to say
    /// so on purpose.
    #[serde(default, deserialize_with = "declared_interrupt")]
    pub interrupt: Option<Option<u32>>,
    pub flags:     u32
}
pub struct Checked
{
    pub id:        String,
    pub bus:       [u8; 4],
    pub key:       [u8; 16],
    pub address:   u64,
    pub bytes:     u64,
    pub interrupt: Option<u32>,
    pub flags:     u32
}

/// Keep "present and null" distinct from "absent": the missing field stays
/// `None`, while an explicit null becomes `Some(None)`.
fn declared_interrupt<'de, D>(deserializer: D)
                             -> std::result::Result<Option<Option<u32>>, D::Error>
    where D: serde::Deserializer<'de>
{
    Option::<u32>::deserialize(deserializer).map(Some)
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
        let interrupt = match d.interrupt
        {
            Some(interrupt) => interrupt,
            None =>
            {
                return Err(error("platform MMIO device must declare its interrupt: a number, \
                                  or null for a block that raises none on the ARM side. The \
                                  field being absent is not the same statement"));
            }
        };
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
                               interrupt,
                               flags: d.flags });
    }
    Ok(checked)
}
/// What a window with no ARM interrupt carries in the generated table. The
/// tuple that consumers read is a plain integer, so absence needs a value; this
/// one is outside every interrupt the controller can raise, and the record is
/// what says which windows mean it.
pub const NO_INTERRUPT: u32 = u32::MAX;
pub fn constants(devices: &[Checked]) -> String
{
    let mut result = String::from("\npub const PLATFORM_MMIO_DEVICES: \
                                   &[([u8;4],[u8;16],u64,u64,u32,u32)] = &[\n");
    for d in devices
    {
        result.push_str(&format!("({:?},{:?},{},{},{},{}),\n",
                                 d.bus,
                                 d.key,
                                 d.address,
                                 d.bytes,
                                 d.interrupt.unwrap_or(NO_INTERRUPT),
                                 d.flags));
    }
    result.push_str("];\n");
    result
}
