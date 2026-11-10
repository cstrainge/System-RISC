//! Shared file-icon resolution and bounded raster export through System's icon cache.
//! No file view supplies its own glyphs or palette. Raster export uses the same
//! descriptor rules and resident plotting services as the graphical file views.
use alloc::{rc::Rc, string::String, vec::Vec};
use core::cell::RefCell;

use mac_toolbox::{Error, Rect, System, ToolboxResult, TypeCode,
                  files::{CatalogInfo, VolumeInfo},
                  quickdraw::{graphics::GraphicsPort,
                              icons::{IconFlags, standard}}};

use crate::rules::{self, Icon};

pub struct IconRaster
{
    pub pixels: [Option<u32>; 256]
}

pub struct IconCache<'a>
{
    system:  System<'a>,
    rasters: RefCell<Vec<(Icon, String, Rc<IconRaster>)>>
}
impl<'a> IconCache<'a>
{
    pub fn new(system: System<'a>) -> Self
    {
        Self { system,
               rasters: RefCell::new(Vec::new()) }
    }

    pub fn resolve_icon_for_file(&self, volume: &VolumeInfo, entry: &CatalogInfo)
                                 -> ToolboxResult<Rc<IconRaster>>
    {
        self.raster(rules::entry_icon(&self.system, volume, entry), entry.name())
    }

    pub fn resolve_icon_for_type(&self, creator: TypeCode, file_type: TypeCode)
                                 -> ToolboxResult<Rc<IconRaster>>
    {
        let fallback = match self.system.quickdraw().icons().for_file(creator, file_type)
        {
            Ok(id) => id,
            Err(Error::NotFound) => standard::DOCUMENT,
            Err(error) => return Err(error)
        };
        self.raster(Icon::Associated { file_type,
                                       fallback },
                    "")
    }

    fn raster(&self, icon: Icon, name: &str) -> ToolboxResult<Rc<IconRaster>>
    {
        let name = if matches!(icon, Icon::Resource(_)) { "" } else { name };
        if let Some((_, _, raster)) =
            self.rasters.borrow().iter().find(|(cached, label, _)| *cached == icon && label == name)
        {
            return Ok(Rc::clone(raster));
        }
        let world = self.system.quickdraw().new_gworld(16, 16)?;
        let port = world.graphics();
        let bounds = Rect::new(0, 0, 16, 16);
        let mut dark = [0; 256];
        let mut light = [0; 256];
        for (background, output) in [(0, &mut dark), (0xffffff, &mut light)]
        {
            port.reset()?;
            port.colors(background, background)?;
            port.paint_rect(bounds)?;
            plot(&self.system, port, icon, name, bounds, IconFlags::default())?;
            if world.pixels(output)? != 256
            {
                return Err(Error::CorruptData);
            }
        }
        // Classic icon families have a binary mask. Pixels left unchanged on
        // both canvases are transparent, including actual black/white glyphs.
        let raster = Rc::new(IconRaster { pixels: core::array::from_fn(|i| {
                                              if dark[i] == 0 && light[i] == 0xffffff
                                              {
                                                  None
                                              }
                                              else
                                              {
                                                  Some(dark[i] & 0xffffff)
                                              }
                                          }) });
        // The listing owns this bounded cache; resident resources remain System-owned.
        let mut rasters = self.rasters.borrow_mut();
        if rasters.len() == 64
        {
            rasters.remove(0);
        }
        rasters.push((icon, String::from(name), Rc::clone(&raster)));
        Ok(raster)
    }
}

/// Resolve and plot a descriptor consistently for every file-list consumer.
pub fn plot(system: &System<'_>, port: GraphicsPort<'_>, icon: Icon, name: &str, bounds: Rect,
            flags: IconFlags)
            -> ToolboxResult<()>
{
    let icons = system.quickdraw().icons();
    let fallback = match icon
    {
        Icon::Resource(id) => id,
        Icon::Application { volume,
                            cnid,
                            fallback } =>
        {
            match icons.plot_file(port.target(), volume, cnid, bounds, flags)
            {
                Ok(()) => return Ok(()),
                Err(Error::NotFound
                    | Error::BadArgument
                    | Error::NotSupported
                    | Error::ServiceBusy) => fallback,
                Err(error) => return Err(error)
            }
        }
        Icon::Document { volume,
                         cnid,
                         file_type,
                         fallback } =>
        {
            match icons.plot_file(port.target(), volume, cnid, bounds, flags)
            {
                Ok(()) => return Ok(()),
                Err(Error::NotFound
                    | Error::BadArgument
                    | Error::NotSupported
                    | Error::ServiceBusy) => (),
                Err(error) => return Err(error)
            }
            match icons.plot_file_default(port.target(), file_type, name, bounds, flags)
            {
                Ok(()) => return Ok(()),
                Err(Error::NotFound | Error::NotSupported) => fallback,
                Err(error) => return Err(error)
            }
        }
        Icon::Associated { file_type,
                           fallback } =>
        {
            match icons.plot_file_default(port.target(), file_type, name, bounds, flags)
            {
                Ok(()) => return Ok(()),
                Err(Error::NotFound | Error::NotSupported) => fallback,
                Err(error) => return Err(error)
            }
        }
    };
    icons.plot(port.target(), fallback, bounds, flags)
}
