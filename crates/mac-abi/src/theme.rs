//! Native role palette snapshot. Applications select roles, never their colors.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Palette
{
    pub revision:        u64,
    pub text:            u32,
    pub content:         u32,
    pub selected_text:   u32,
    pub selection:       u32,
    pub secondary_text:  u32,
    pub disabled_text:   u32,
    pub control_outline: u32,
    pub progress_fill:   u32,
    pub progress_track:  u32,
    pub scrollbar:       ScrollbarColors,
    pub separator:       u32
}

/// Named shades for the original scrollbar sprite vocabulary. The sprites
/// contain shape indices; their colors come from the resident theme snapshot.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrollbarColors
{
    pub ink:          u32,
    pub accent_dark:  u32,
    pub gray_dark:    u32,
    pub accent_mid:   u32,
    pub gray_mid:     u32,
    pub accent_light: u32,
    pub face:         u32,
    pub accent_pale:  u32,
    pub gray_light:   u32,
    pub face_light:   u32,
    pub highlight:    u32
}
impl ScrollbarColors
{
    pub const fn sprite_palette(self) -> [u32; 11]
    {
        [self.ink, self.accent_dark, self.gray_dark, self.accent_mid, self.gray_mid,
         self.accent_light, self.face, self.accent_pale, self.gray_light, self.face_light,
         self.highlight]
    }
}
