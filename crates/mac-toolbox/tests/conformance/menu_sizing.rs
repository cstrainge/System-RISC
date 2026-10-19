//! Uncached standard-menu geometry captured from original CalcMenuSize calls.
use mac_toolbox::{BAD_ARGUMENT, Error, System, ToolboxResult};
pub const CASES: &[(i16, &str, i16, i16)] = &[(31000, "keyless_close", 115, 16),
                                              (31001, "close_W", 139, 16),
                                              (31002, "folder_N", 122, 16),
                                              (31003, "short_I", 52, 16),
                                              (31004, "short_W", 52, 16),
                                              (31005, "finder_window", 139, 32),
                                              (31006, "finder_desktop", 139, 32),
                                              (31007, "long_keyless", 177, 16)];
pub fn run(system: &System<'_>) -> ToolboxResult<()>
{
    system.menus().clear()?;
    for &(id, _, width, height) in CASES
    {
        system.menus().install_menu_resource(1, id)?;
        let bounds = system.menus().layout(id)?.popup;
        if bounds.right - bounds.left != width || bounds.bottom - bounds.top != height
        {
            return Err(Error(BAD_ARGUMENT));
        }
    }
    Ok(())
}
