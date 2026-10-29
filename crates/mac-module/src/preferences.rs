//! MPRE v1: bounded versioned JSON in the admitted module's own Preferences
//! file. Compare/exchange compares exact previous bytes under the File Manager
//! transaction boundary; a stale client cannot silently overwrite newer policy.
use crate::ClassId;
pub const CLASS: ClassId = ClassId(*b"MPRE");
pub const VERSION: u32 = 1;
pub const LOAD: u32 = 1;
pub const COMPARE_EXCHANGE: u32 = 2;
/// Startup catalog change sequence for cheap unchanged-policy observation.
pub const REVISION: u32 = 3;
pub const MAX_BYTES: usize = 256;
pub const REPLY_BYTES: usize = MAX_BYTES + 1;
pub const MAX_REQUEST: usize = 4 + 2 * MAX_BYTES;
pub const ABSENT: u32 = u32::MAX;

/// Stable persistent key; module display-name changes cannot redirect storage.
/// Runtime authorization still comes from the actual admitted module owner.
pub fn filename(identity: [u8; 16]) -> [u8; 37]
{
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut name = [0; 37];
    for (index, byte) in identity.into_iter().enumerate()
    {
        name[2 * index] = HEX[(byte >> 4) as usize];
        name[2 * index + 1] = HEX[(byte & 15) as usize];
    }
    name[32..].copy_from_slice(b".json");
    name
}
