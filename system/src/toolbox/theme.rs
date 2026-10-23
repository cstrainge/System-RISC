//! Resident native theme roles. Literal palette policy stays here, out of apps.
#![forbid(unsafe_code)]
pub(crate) fn palette() -> mac_abi::theme::Palette
{
    mac_abi::theme::Palette { revision:        1,
                              text:            0,
                              content:         0xffffff,
                              selected_text:   0xffffff,
                              selection:       0x333399,
                              secondary_text:  0x555555,
                              disabled_text:   0xa5a5a5,
                              control_outline: 0,
                              progress_fill:   0x666666,
                              progress_track:  0xdadaff,
                              scrollbar:       mac_abi::theme::ScrollbarColors { ink:          0,
                                                                                 accent_dark:
                                                                                     0x545487,
                                                                                 gray_dark:
                                                                                     0x777777,
                                                                                 accent_mid:
                                                                                     0x8787b3,
                                                                                 gray_mid:
                                                                                     0x969696,
                                                                                 accent_light:
                                                                                     0xb3b3da,
                                                                                 face:
                                                                                     0xc0c0c0,
                                                                                 accent_pale:
                                                                                     0xdadaff,
                                                                                 gray_light:
                                                                                     0xe7e7e7,
                                                                                 face_light:
                                                                                     0xf3f3f3,
                                                                                 highlight:
                                                                                     0xffffff },
                              separator:       0x888888 }
}
