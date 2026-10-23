//! Native role palette snapshot. Applications select roles, never their colors.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Palette
{
    pub revision:       u64,
    pub text:           u32,
    pub content:        u32,
    pub selected_text:  u32,
    pub selection:      u32,
    pub secondary_text: u32,
    pub separator:      u32
}
