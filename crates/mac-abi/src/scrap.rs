//! Native shared clipboard requests. Text has a distinct UTF-8 operation;
//! untyped binary access to TEXT is deliberately rejected by the native gate.
pub const INFO: u32 = 0;
pub const CLEAR: u32 = 1;
pub const PUT: u32 = 2;
pub const READ: u32 = 3;
pub const FLAVOR_AT: u32 = 4;
pub const PUT_TEXT: u32 = 5;
pub const READ_TEXT: u32 = 6;
pub const LOAD: u32 = 7;
pub const UNLOAD: u32 = 8;
pub const TEXT: [u8; 4] = *b"TEXT";
/// Original GetScrap missing-flavor result.
pub const NO_TYPE_ERR: i32 = -102;
/// Native policy bounds, rather than original System heap limits.
pub const MAX_BYTES: usize = 1024 * 1024;
pub const MAX_FLAVORS: usize = 256;
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrapInfo
{
    /// Total record bytes: eight-byte headers and even-padded flavor payloads.
    /// Native TEXT payloads count UTF-8 bytes, not MacRoman bytes.
    pub size:    u32,
    /// Classic wrapping count advanced by a successful clear, not by puts.
    pub count:   u16,
    /// Number of stored records, including repeated flavor records.
    pub flavors: u16
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlavorInfo
{
    pub kind:   [u8; 4],
    pub length: u32,
    /// Offset of payload following its header in the logical scrap record data.
    pub offset: u32
}
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ScrapCall
{
    pub operation:    u32,
    pub kind:         [u8; 4],
    /// Zero-based record index for FLAVOR_AT.
    pub index:        u32,
    /// Native partial-read byte offset for binary READ only.
    pub offset:       u32,
    pub input:        *const u8,
    pub input_length: usize,
    pub output:       *mut u8,
    pub capacity:     usize,
    pub transferred:  usize,
    pub info:         ScrapInfo,
    pub flavor:       FlavorInfo
}
impl Default for ScrapCall
{
    fn default() -> Self
    {
        Self { operation:    0,
               kind:         [0; 4],
               index:        0,
               offset:       0,
               input:        core::ptr::null(),
               input_length: 0,
               output:       core::ptr::null_mut(),
               capacity:     0,
               transferred:  0,
               info:         ScrapInfo::default(),
               flavor:       FlavorInfo::default() }
    }
}
