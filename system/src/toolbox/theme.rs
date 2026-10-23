//! Resident native theme roles. Literal palette policy stays here, out of apps.
#![forbid(unsafe_code)]
pub(crate) fn palette() -> mac_abi::theme::Palette
{
    mac_abi::theme::Palette { revision:       1,
                              text:           0,
                              content:        0xffffff,
                              selected_text:  0xffffff,
                              selection:      0x333399,
                              secondary_text: 0x555555,
                              separator:      0x888888 }
}
