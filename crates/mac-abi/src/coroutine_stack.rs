//! Guarded user stack mappings owned by the calling native thread.
pub const ALLOCATE: u32 = 0;
pub const RELEASE: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Call
{
    pub operation: u32,
    pub reserved:  u32,
    pub bytes:     u64,
    pub base:      usize,
    pub top:       usize,
    /// Lowest address of the inaccessible lower guard page on allocation.
    pub limit:     usize
}
