//! Finder observes folders without doing storage I/O on pointer/key paths.
use mac_toolbox::{Error, System, WindowId, files::DirectoryWatch};

use crate::browser::{Browser, Location, desktop};
struct Watched<'a>
{
    window:   WindowId,
    location: Location,
    watch:    DirectoryWatch<'a>
}
pub struct Controller<'a>
{
    folders: [Option<Watched<'a>>; 8],
    desktop: [Option<Watched<'a>>; 128]
}
impl Default for Controller<'_>
{
    fn default() -> Self
    {
        Self { folders: core::array::from_fn(|_| None),
               desktop: core::array::from_fn(|_| None) }
    }
}
impl<'a> Controller<'a>
{
    pub fn poll(&mut self, system: &System<'a>, browser: &mut Browser) -> Result<(), Error>
    {
        let desktop_folders = desktop::folders();
        for watch in &mut self.desktop
        {
            if watch.as_ref().is_some_and(|w| !desktop_folders.contains(&Some(w.location)))
            {
                *watch = None;
            }
        }
        for location in desktop_folders.into_iter().flatten()
        {
            if self.desktop.iter().flatten().any(|w| w.location == location)
            {
                continue;
            }
            let slot = self.desktop.iter_mut().find(|w| w.is_none()).ok_or(Error::NoMemory)?;
            *slot = Some(Watched { window: WindowId(0),
                                   location,
                                   watch:
                                       system.files()
                                             .watch_directory(location.volume, location.folder)? });
        }
        let mut changed = false;
        for watch in self.desktop.iter().flatten()
        {
            changed |= watch.watch.poll()?.is_some();
        }
        if changed
        {
            desktop::refresh();
        }
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
