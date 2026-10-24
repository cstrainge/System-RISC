//! Standard File Package native requests. System owns modal UI and event handling.
use crate::files::CatalogInfo;
pub const BEGIN: u32 = 0;
pub const POLL: u32 = 1;
pub const DISPOSE: u32 = 2;
pub const GET: u32 = 0;
pub const PUT: u32 = 1;
pub const PENDING: u32 = 0;
pub const ACCEPTED: u32 = 1;
pub const CANCELLED: u32 = 2;
pub const MAX_TYPES: usize = 16;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct StandardFileCall
{
    pub operation:     u32,
    pub handle:        u64,
    pub mode:          u32,
    pub volume:        u32,
    pub directory:     u32,
    /// -1 shows all types, including invisible entries, as StandardGetFile does.
    pub type_count:    i32,
    pub types:         [[u8; 4]; MAX_TYPES],
    pub prompt:        [u8; 768],
    pub prompt_length: u32,
    pub name:          [u8; 768],
    pub name_length:   u32,
    pub state:         u32,
    pub replacing:     u32,
    pub entry:         CatalogInfo,
    pub reserved:      [u64; 2]
}
impl Default for StandardFileCall
{
    fn default() -> Self
    {
        Self { operation:     BEGIN,
               handle:        0,
               mode:          GET,
               volume:        0,
               directory:     0,
               type_count:    -1,
               types:         [[0; 4]; MAX_TYPES],
               prompt:        [0; 768],
               prompt_length: 0,
               name:          [0; 768],
               name_length:   0,
               state:         PENDING,
               replacing:     0,
               entry:         CatalogInfo::default(),
               reserved:      [0; 2] }
    }
}
