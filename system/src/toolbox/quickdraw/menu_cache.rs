//! One bounded snapshot of the foreground menu bar, independent of windows.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use mac_object_table::shared::InvocationGate as _;

use super::Canvas;
use crate::toolbox::handles::InvocationGate;
pub const BYTES: usize = crate::display::MAX_WIDTH * 21 * 4;

#[derive(Default)]
pub struct MenuBarCache
{
    key:        Option<(u64, Option<i16>, bool, bool)>,
    pixels:     Vec<u32>,
    pub paints: u64
}
impl MenuBarCache
{
    pub fn invalidate(&mut self)
    {
        self.key = None;
    }

    /// The full compositor supplies screen coordinates and a complete surface.
    /// Popups remain separate overlays; hover rows cannot invalidate bar pixels.
    pub fn paint<E>(&mut self, canvas: &mut Canvas<'_>, key: (u64, Option<i16>, bool, bool),
                    draw: impl FnOnce(&mut Canvas<'_>) -> Result<(), E>)
                    -> Result<(), E>
    {
        // Cache shape follows the active stride, never the maximum reserved
        // geometry. A width change cannot replay old row layout.
        let count = canvas.width * mac_abi::menus::MENU_BAR_HEIGHT as usize;
        if self.key == Some(key) && self.pixels.len() == count
        {
            canvas.pixels[..count].copy_from_slice(&self.pixels);
            return Ok(());
        }
        self.key = None;
        draw(canvas)?;
        self.paints = self.paints.wrapping_add(1);
        if self.pixels.len() != count
        {
            if InvocationGate::with_system_allocation(|| {
                   self.pixels.try_reserve_exact(count.saturating_sub(self.pixels.len()))
               }).is_err()
            {
                return Ok(());
            }
            self.pixels.resize(count, 0);
        }
        self.pixels.copy_from_slice(&canvas.pixels[..count]);
        self.key = Some(key);
        Ok(())
    }
}
