//! Finder observes folders without doing storage I/O on pointer/key paths.
use mac_toolbox::{Error, System, WindowId, files::DirectoryWatch};

use crate::browser::{Browser, Location};
struct Watched<'a>
{
    window:   WindowId,
    location: Location,
    watch:    DirectoryWatch<'a>
}
pub struct Controller<'a>
{
    folders: [Option<Watched<'a>>; 8]
}
impl Default for Controller<'_>
{
    fn default() -> Self
    {
        Self { folders: core::array::from_fn(|_| None) }
    }
}
impl<'a> Controller<'a>
{
    pub fn poll(&mut self, system: &System<'a>, browser: &mut Browser) -> Result<(), Error>
    {
        for slot in &mut self.folders
        {
            if slot.as_ref().is_some_and(|w| {
                                !browser.folder_locations()
                                        .any(|(window, location)| {
                                            window == w.window && location == w.location
                                        })
                            })
            {
                *slot = None;
            }
        }
        for (window, location) in browser.folder_locations()
        {
            if self.folders.iter().flatten().any(|w| w.window == window)
            {
                continue;
            }
            let slot = self.folders.iter_mut().find(|w| w.is_none()).ok_or(Error::NoMemory)?;
            *slot = Some(Watched { window,
                                   location,
                                   watch:
                                       system.files()
                                             .watch_directory(location.volume, location.folder)? });
        }
        for entry in self.folders.iter().flatten()
        {
            if entry.watch.poll()?.is_some()
            {
                browser.folder_changed(system, entry.window)?;
            }
        }
        Ok(())
    }
}
