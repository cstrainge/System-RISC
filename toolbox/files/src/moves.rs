//! Atomic same-volume icon-group moves, with source-parent revalidation.
use super::*;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Placement
{
    #[default]
    Keep,
    At(mac_abi::Point),
    Clear
}
#[derive(Clone, Copy, Debug, Default)]
pub struct IconMove
{
    pub id:              Cnid,
    pub expected_parent: Cnid,
    pub parent:          Cnid,
    pub placement:       Placement,
    pub to_trash:        bool
}
impl Files<'_>
{
    pub fn move_icons(self, volume: VolumeId, entries: &[IconMove]) -> Result<bool>
    {
        self.move_icons_at(volume, entries, self.client.now()?)
    }

    pub fn move_icons_at(self, volume: VolumeId, entries: &[IconMove], when: Timestamp)
                         -> Result<bool>
    {
        if entries.len() > 1024
        {
            return Err(Error::NoMemory);
        }
        let mut rows = [mac_abi::files::IconMove::default(); 1024];
        for (row, entry) in rows.iter_mut().zip(entries)
        {
            let (placement, point) = match entry.placement
            {
                Placement::Keep => (0, mac_abi::Point::default()),
                Placement::At(p) => (1, p),
                Placement::Clear => (2, mac_abi::Point::default())
            };
            *row = mac_abi::files::IconMove { cnid: entry.id.0,
                                              expected_parent: entry.expected_parent.0,
                                              parent: entry.parent.0,
                                              placement: placement
                                                         | if entry.to_trash { 4 } else { 0 },
                                              point };
        }
        let mut call = mac_abi::files::IconMoveCall { operation: mac_abi::files::MOVE_ICONS,
                                                      volume: volume.0,
                                                      when: when.mac_microseconds()
                                                                .ok_or(Error::BadArgument)?,
                                                      ..Default::default() };
        self.client.move_icons(&mut call, &rows[..entries.len()])?;
        Ok(call.changed != 0)
    }
}
