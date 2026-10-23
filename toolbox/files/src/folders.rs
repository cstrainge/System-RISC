//! Persisted directory-window geometry using the original Finder fields.
use mac_abi::{Point, Rect, files::*};

use super::{CatalogInfo, Cnid, Error, Files, Result, Timestamp, VolumeId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FolderState
{
    pub bounds: Rect,
    pub scroll: Point,
    pub view:   u16
}
impl FolderState
{
    pub fn from_catalog(entry: &CatalogInfo) -> Option<Self>
    {
        let state = Self { bounds: entry.folder_window_bounds()?,
                           scroll: entry.folder_scroll()?,
                           view:   entry.folder_view()? };
        (state.bounds.right > state.bounds.left && state.bounds.bottom > state.bounds.top)
            .then_some(state)
    }
}
impl Files<'_>
{
    /// Save only the window rectangle, view and scroll origin. Other Finder
    /// fields, file placement, object dates, comments and forks are preserved.
    pub fn set_folder_state(self, volume: VolumeId, folder: Cnid, state: FolderState)
                            -> Result<CatalogInfo>
    {
        self.set_folder_state_at(volume, folder, state, self.client.now()?)
    }

    pub fn set_folder_state_at(self, volume: VolumeId, folder: Cnid, state: FolderState,
                               when: Timestamp)
                               -> Result<CatalogInfo>
    {
        if state.bounds.right <= state.bounds.left
           || state.bounds.bottom <= state.bounds.top
           || folder.0 < ROOT_ID
        {
            return Err(Error::BadArgument);
        }
        let mut call = FileCall { operation: SET_FOLDER_STATE,
                                  volume: volume.0,
                                  cnid: folder.0,
                                  position: when.mac_microseconds().ok_or(Error::BadArgument)?,
                                  ..Default::default() };
        call.entry.finder_info[..8].copy_from_slice(&state.bounds.to_be_bytes());
        call.entry.finder_info[14..16].copy_from_slice(&state.view.to_be_bytes());
        call.entry.finder_info[16..18].copy_from_slice(&state.scroll.v.to_be_bytes());
        call.entry.finder_info[18..20].copy_from_slice(&state.scroll.h.to_be_bytes());
        self.call(call, "")?.entry.try_into()
    }
}
