
#[repr(C)]
pub struct IconSet
{
    pub id: i16,
    pub small_mono: [u8; 32],
    pub small_mask: [u8; 32],
    pub small_four: [u8; 128],
    pub small_eight: [u8; 256],
    pub large_mono: [u8; 128],
    pub large_mask: [u8; 128],
    pub large_four: [u8; 512],
    pub large_eight: [u8; 1024]
}
