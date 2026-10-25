//! Process identities are assigned by the scheduler, never supplied as authority.
use crate::*;
pub const CURRENT: u32 = 0;
pub const COUNT: u32 = 1;
pub const INDEX: u32 = 2;
pub const LAUNCH: u32 = 3;
pub const ACTIVATE: u32 = 4;
pub const HIDE: u32 = 5;
pub const HIDE_OTHERS: u32 = 6;
pub const SHOW_ALL: u32 = 7;
pub const EXIT: u32 = 8;
/// Exact mounted-volume/catalog identity, independent of path or creator code.
pub const IS_RUNNING: u32 = 9;
/// Native lifecycle notification: payload packs volume low/CNID high; message
/// low bit reports whether this exact file still has a running instance.
pub const FILE_STATE_EVENT: u32 = 0x0c00_0000;
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ProcessInfo
{
    pub id:          u64,
    pub thread:      u64,
    /// Readonly image alias identity; zero while image admission is incomplete.
    pub image:       u64,
    pub volume:      u32,
    pub cnid:        u32,
    pub foreground:  u32,
    pub hidden:      u32,
    pub name:        [u8; INLINE_STRING_BYTES],
    pub name_length: u32,
    pub switches:    u64
}
impl Default for ProcessInfo
{
    fn default() -> Self
    {
        Self { id:          0,
               thread:      0,
               image:       0,
               volume:      0,
               cnid:        0,
               foreground:  0,
               hidden:      0,
               name:        [0; INLINE_STRING_BYTES],
               name_length: 0,
               switches:    0 }
    }
}
impl ProcessInfo
{
    pub fn name(&self) -> &str
    {
        core::str::from_utf8(&self.name[..(self.name_length as usize).min(self.name.len())]).unwrap_or("")
    }
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct ProcessCall
{
    pub operation: u32,
    pub index:     u32,
    pub id:        u64,
    pub volume:    u32,
    pub cnid:      u32,
    pub info:      ProcessInfo
}

/// Size-distinct native document delivery. Sender owns a preparing batch;
/// Commit transfers it atomically to the exact application's incoming queue.
pub const DOCUMENT_BEGIN: u32 = 0;
pub const DOCUMENT_PUSH: u32 = 1;
pub const DOCUMENT_COMMIT: u32 = 2;
pub const DOCUMENT_ABORT: u32 = 3;
pub const DOCUMENT_ACCEPTS: u32 = 4;
pub const DOCUMENT_NEXT: u32 = 5;
pub const DOCUMENT_AT: u32 = 6;
pub const DOCUMENT_FINISH: u32 = 7;
pub const DOCUMENT_ACCEPTS_DRAG: u32 = 8;
pub const DOCUMENT_AVAILABLE: u32 = 0x1000_0000;
pub const MAX_DOCUMENTS: usize = 1024;
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct DocumentCall
{
    pub drag:               u32,
    pub operation:          u32,
    pub handle:             u64,
    pub application_volume: u32,
    pub application_cnid:   u32,
    pub volume:             u32,
    pub cnid:               u32,
    pub file_type:          [u8; 4],
    pub index:              u32,
    pub count:              u32,
    pub present:            u32,
    pub reserved:           [u32; 2],
    pub entry:              crate::files::CatalogInfo,
    pub process:            ProcessInfo
}

#[cfg(test)]
mod document_layout_tests
{
    use super::*;
    #[test]
    fn native_document_ids_have_full_width_and_distinct_request_layout()
    {
        assert_eq!(core::mem::offset_of!(DocumentCall, handle), 8);
        assert_eq!(core::mem::offset_of!(DocumentCall, application_volume), 16);
        assert_ne!(core::mem::size_of::<DocumentCall>(), core::mem::size_of::<ProcessCall>());
        let call = DocumentCall { handle: u64::from(u32::MAX) + 1,
                                  ..Default::default() };
        assert!(call.handle > u64::from(u32::MAX));
    }
}
