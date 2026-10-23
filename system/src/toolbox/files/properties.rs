//! Full metadata projection shared by native and converted legacy requests.
#![forbid(unsafe_code)]
use mac_abi::{files::*, *};
use mac_filesystem_interface::MetadataPatch;
// An internal continuation discriminator, never an accepted public FileCall.
#[cfg(target_arch = "riscv64")]
pub(super) const RESTORE_MODIFIED: u32 = u32::MAX;
#[cfg(target_arch = "riscv64")]
pub(super) const CREATE_PRIVATE_DIRECTORY: u32 = u32::MAX - 1;
pub(super) fn validate(call: &FileCall, comment: &str) -> Result<(), Status>
{
    let e = &call.entry;
    if call.cnid <= ROOT_ID
       || call.index != 0
       || call.fork != 0
       || comment.len() > COMMENT_MAX_BYTES
       || e.backup_present > 1
       || e.backup_present == 0 && e.backup != 0
       || e.volume != 0
       || e.id != 0
       || e.parent != 0
       || e.kind != 0
       || e.data_length != 0
       || e.resource_length != 0
       || e.file_type != [0; 4]
       || e.creator != [0; 4]
       || e.child_count != 0
       || e.name_length != 0
       || e.name != [0; 256]
    {
        return Err(BAD_ARGUMENT);
    }
    Ok(())
}
pub(super) fn patch<'a>(call: &FileCall, comment: &'a str) -> Result<MetadataPatch<'a>, Status>
{
    validate(call, comment)?;
    Ok(MetadataPatch { attributes_mask: u16::MAX,
                       attributes:      call.entry.flags,
                       finder_mask:     [255; 32],
                       finder_info:     call.entry.finder_info,
                       created:         Some(call.entry.created),
                       modified:        Some(call.entry.modified),
                       backup:
                           Some((call.entry.backup_present != 0).then_some(call.entry.backup)),
                       comment:         Some(comment) })
}
pub(in crate::toolbox::files) fn finder_key(info: [u8; 32]) -> [u8; 64]
{
    let mut key = [0; 64];
    key[..32].copy_from_slice(&info);
    key
}
pub(in crate::toolbox::files) fn key(call: &FileCall) -> Option<[u8; 64]>
{
    match call.operation
    {
        SET_FOLDER_STATE => Some(finder_key(call.entry.finder_info)),
        SET_PROPERTIES =>
        {
            let e = &call.entry;
            let mut key = [0; 64];
            key[..32].copy_from_slice(&e.finder_info);
            key[32..40].copy_from_slice(&e.created.to_le_bytes());
            key[40..48].copy_from_slice(&e.modified.to_le_bytes());
            key[48..56].copy_from_slice(&e.backup.to_le_bytes());
            key[56..60].copy_from_slice(&e.backup_present.to_le_bytes());
            key[60..62].copy_from_slice(&e.flags.to_le_bytes());
            Some(key)
        }
        _ => None
    }
}
