//! Classic icon family/resource lookup and drawing are System services.
use mac_abi::{quickdraw::*,
              resources::{APPLICATION, CHAIN, SYSTEM}};
use mac_traps::{Cnid, TypeCode, VolumeId};

use crate::{Client, Rect, Result, WindowId};
#[derive(Clone, Copy, Default)]
pub struct IconFlags
{
    pub selected: bool,
    pub disabled: bool,
    /// Replace source colors with the classic open-icon silhouette/stipple.
    pub open:     bool
}
impl IconFlags
{
    fn bits(self) -> Result<u16>
    {
        if self.open && self.disabled
        {
            return Err(mac_traps::Error::BadArgument);
        }
        Ok((if self.open { 3 } else { u16::from(self.disabled) })
           | if self.selected { 0x4000 } else { 0 })
    }
}
#[derive(Clone, Copy)]
pub struct Icons<'a>
{
    client: Client<'a>,
    scope:  i16
}
impl<'a> Icons<'a>
{
    pub const fn new(client: Client<'a>) -> Self
    {
        Self { client,
               scope: CHAIN }
    }

    pub const fn system(self) -> Self
    {
        Self { scope: SYSTEM,
               ..self }
    }

    pub const fn application(self) -> Self
    {
        Self { scope: APPLICATION,
               ..self }
    }

    pub fn count(self) -> Result<u32>
    {
        Ok(self.client
               .icon(IconCall { operation: ICON_COUNT,
                                scope: self.scope,
                                ..Default::default() })?
               .index)
    }

    pub fn at(self, index: u32) -> Result<i16>
    {
        Ok(self.client
               .icon(IconCall { operation: ICON_AT,
                                scope: self.scope,
                                index,
                                ..Default::default() })?
               .id)
    }

    pub fn for_file(self, creator: TypeCode, file_type: TypeCode) -> Result<i16>
    {
        Ok(self.client
               .icon(IconCall { operation: ICON_FILE,
                                scope: self.scope,
                                creator: creator.bytes(),
                                file_type: file_type.bytes(),
                                ..Default::default() })?
               .id)
    }

    pub fn plot(self, window: WindowId, id: i16, destination: Rect, flags: IconFlags)
                -> Result<()>
    {
        self.client
            .icon(IconCall { operation: ICON_PLOT,
                             window: window.0,
                             scope: self.scope,
                             id,
                             destination,
                             flags: flags.bits()?,
                             ..Default::default() })
            .map(|_| ())
    }

    /// Read the file's BNDL/FREF mapping and draw its own icon family. This works
    /// before the application has been launched and leaves the search chain alone.
    pub fn plot_file(self, window: WindowId, volume: VolumeId, cnid: Cnid, destination: Rect,
                     flags: IconFlags)
                     -> Result<()>
    {
        self.client.file_icon(&mut FileIconCall { operation: ICON_PLOT_FILE,
                                                  window: window.0,
                                                  volume: volume.0,
                                                  cnid: cnid.0,
                                                  destination,
                                                  flags: flags.bits()?,
                                                  ..Default::default() })
    }
}
impl<'a> crate::QuickDraw<'a>
{
    pub fn icons(self) -> Icons<'a>
    {
        Icons::new(self.client)
    }
}
impl crate::SurfacePort<'_>
{
    pub fn plot_file_icon(&mut self, volume: VolumeId, cnid: Cnid, destination: Rect,
                          flags: IconFlags)
                          -> Result<()>
    {
        self.connection.file_icon(volume.0, cnid.0, destination, flags.bits()?)
    }

    pub fn plot_icon(&mut self, scope: mac_traps::types::ResourceScope, id: i16,
                     destination: Rect, flags: IconFlags)
                     -> Result<()>
    {
        self.connection.icon(scope.raw(), id, destination, flags.bits()?)
    }
}
/// Well-known System icon families, including original Macintosh IDs and native additions.
pub mod standard
{
    pub const DOCUMENT: i16 = -4000;
    pub const FOLDER: i16 = -3999;
    pub const FLOPPY: i16 = -3998;
    pub const APPLICATION: i16 = -3996;
    pub const TRASH: i16 = -3993;
    pub const FULL_TRASH: i16 = -3984;
    pub const SYSTEM_FOLDER: i16 = -3983;
    pub const APPLE_MENU_FOLDER: i16 = -3982;
    pub const STARTUP_FOLDER: i16 = -3981;
    pub const CONTROL_PANELS_FOLDER: i16 = -3976;
    pub const PREFERENCES_FOLDER: i16 = -3974;
    pub const EXTENSIONS_FOLDER: i16 = -3973;
    pub const FONTS_FOLDER: i16 = -3968;
    /// System RISC's Drivers folder, with its own large/small color/mono family.
    pub const DRIVERS_FOLDER: i16 = 32730;
}

impl crate::graphics::GraphicsPort<'_>
{
    pub fn draw_icon(self, kind: [u8; 4], data: &[u8], destination: Rect) -> Result<()>
    {
        self.client.icon_data(&mut IconDataCall { window: self.window.0,
                                                  kind,
                                                  destination,
                                                  ..Default::default() },
                              data)
    }
}
