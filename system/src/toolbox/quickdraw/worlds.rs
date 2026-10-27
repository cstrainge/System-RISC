//! Owner-scoped offscreen ports. The shared facility owns every identity and
//! backing lifetime; applications never receive storage pointers.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use mac_abi::*;

use super::{Canvas,
            fonts::FontManager,
            fonts_service,
            service::{self, Port}};
use crate::toolbox::handles::{CapacityPolicy, KindCode, KindLease, OwnerRef, Store};
// Existing per-application raster budget, pending the accounted page-heap stage.
const MAX_PIXELS: usize = 512 * 1024;
pub struct World
{
    pub width:  usize,
    pub height: usize,
    pub pixels: Vec<u32>,
    pub port:   Port
}
struct ApplicationState
{
    pixels: usize
}
pub struct Worlds
{
    worlds: Store<World, ApplicationState>
}
pub(crate) fn specification(state: bool) -> mac_object_table::registry::Specification<'static>
{
    mac_object_table::registry::Specification { kind:     KindCode(if state
                                                                   {
                                                                       *b"GWST"
                                                                   }
                                                                   else
                                                                   {
                                                                       *b"GWLD"
                                                                   }),
                                                label:    if state
                                                {
                                                    "Offscreen port state"
                                                }
                                                else
                                                {
                                                    "Offscreen worlds"
                                                },
                                                provider: "System QuickDraw",
                                                capacity: if state
                                                {
                                                    CapacityPolicy::HardBound { slots:  1,
                                                                                reason:
                                                                                    "One offscreen \
                                                                                     port state \
                                                                                     per domain" }
                                                }
                                                else
                                                {
                                                    CapacityPolicy::GrowableUntilMemory
                                                },
                                                source:
                                                    mac_object_table::registry::Source::Resident }
}
pub(crate) trait WorldHost
{
    fn offscreen_worlds(&mut self) -> &mut Worlds;
}
pub(crate) struct ReleaseWorlds;
pub(crate) struct ReleaseStorage;
impl<C: WorldHost> mac_object_table::registry::Release<C> for ReleaseWorlds
{
    fn release(&self, context: &mut C, ticket: mac_object_table::domain::ReleasedResource)
               -> Result<(), mac_object_table::domain::CompletionError>
    {
        let manager = context.offscreen_worlds();
        let owner = ticket.key().owner().incarnation();
        let world = manager.worlds.release(ticket)?;
        if let Some(state) = manager.worlds.state_mut(owner)
        {
            state.pixels -= world.pixels.len();
        }
        drop(world);
        Ok(())
    }
}
impl<C: WorldHost> mac_object_table::registry::Release<C> for ReleaseStorage
{
    fn release(&self, context: &mut C, ticket: mac_object_table::domain::ReleasedResource)
               -> Result<(), mac_object_table::domain::CompletionError>
    {
        context.offscreen_worlds().worlds.release_storage(ticket)
    }
}
impl Worlds
{
    pub(crate) fn new((objects, state): (KindLease, KindLease)) -> Result<Self, Status>
    {
        Ok(Self { worlds: Store::owned_state(objects, state, |_| Ok(ApplicationState { pixels: 0 })).map_err(|_| BAD_ARGUMENT)? })
    }

    pub(crate) fn create(&mut self, owner: &OwnerRef, width: i16, height: i16)
                         -> Result<u64, Status>
    {
        if width <= 0 || height <= 0
        {
            return Err(BAD_ARGUMENT);
        }
        if !owner.accepting()
        {
            return Err(SERVICE_BUSY);
        }
        let size = width as usize * height as usize;
        let used = self.worlds.state(owner.owner().incarnation()).map_or(0, |state| state.pixels);
        if size > crate::display::PIXELS || used + size > MAX_PIXELS
        {
            return Err(NO_MEMORY);
        }
        let mut pixels = Vec::new();
        pixels.try_reserve_exact(size).map_err(|_| NO_MEMORY)?;
        pixels.resize(size, 0xffffff);
        let id = self.worlds
                     .insert(owner,
                             World { width: width as usize,
                                     height: height as usize,
                                     pixels,
                                     port: Port::default() })
                     .map_err(|error| {
                         match error
                    {
                        mac_object_table::domain::PoolError::Stopping { .. }
                        | mac_object_table::domain::PoolError::RegistrationUnavailable { .. } =>
                        {
                            SERVICE_BUSY
                        }
                        _ => NO_MEMORY
                    }
                     })?;
        self.worlds.state_mut(owner.owner().incarnation()).ok_or(SERVICE_BUSY)?.pixels += size;
        Ok(id)
    }

    pub fn get(&self, id: u64, owner: u64) -> Result<&World, Status>
    {
        let world = self.worlds.get(id).ok_or(NOT_FOUND)?;
        if self.worlds.owner(id).map(|owner| owner.incarnation()) != Some(owner)
        {
            return Err(ACCESS_DENIED);
        }
        Ok(world)
    }

    pub fn get_mut(&mut self, id: u64, owner: u64) -> Result<&mut World, Status>
    {
        self.get(id, owner)?;
        self.worlds.get_mut(id).ok_or(NOT_FOUND)
    }

    pub fn dispose(&mut self, id: u64, owner: u64) -> Result<(), Status>
    {
        let size = self.get(id, owner)?.pixels.len();
        if let Some(state) = self.worlds.state_mut(owner)
        {
            state.pixels -= size;
        }
        drop(self.worlds.remove(id).ok_or(NOT_FOUND)?);
        Ok(())
    }

    pub(crate) fn request(&mut self, owner: &OwnerRef, call: &mut quickdraw::WorldCall,
                          pixels: &mut [u32])
                          -> Result<(), Status>
    {
        use quickdraw::*;
        if call.reserved != 0
        {
            return Err(BAD_ARGUMENT);
        }
        call.target.decode().map_err(|_| BAD_ARGUMENT)?;
        let caller = owner.owner().incarnation();
        match call.operation
        {
            WORLD_NEW => call.world = self.create(owner, call.dimensions.h, call.dimensions.v)?,
            WORLD_DISPOSE =>
            {
                self.dispose(call.world, caller)?;
                return Ok(());
            }
            WORLD_INFO => (),
            WORLD_READ | WORLD_WRITE =>
            {
                let world = self.get_mut(call.world, caller)?;
                call.pixel_length = world.pixels.len();
                if pixels.len() < call.pixel_length
                {
                    return Err(NO_MEMORY);
                }
                if call.operation == WORLD_READ
                {
                    pixels[..call.pixel_length].copy_from_slice(&world.pixels);
                }
                else
                {
                    for (out, input) in world.pixels.iter_mut().zip(pixels.iter())
                    {
                        *out = *input & 0xffffff;
                    }
                }
            }
            _ => return Err(NOT_SUPPORTED)
        }
        let world = self.get(call.world, caller)?;
        call.dimensions = Point { h: world.width as i16,
                                  v: world.height as i16 };
        call.pixel_length = world.pixels.len();
        Ok(())
    }
}
impl World
{
    pub(crate) fn copy_request(&self, call: &quickdraw::WorldCall)
                               -> Result<(quickdraw::BitmapCall, Vec<u8>), Status>
    {
        let mut data = Vec::new();
        data.try_reserve_exact(self.pixels.len() * 4).map_err(|_| NO_MEMORY)?;
        for pixel in &self.pixels
        {
            data.extend_from_slice(&pixel.to_ne_bytes());
        }
        Ok((quickdraw::BitmapCall { target: call.target,
                                    format: 32,
                                    row_bytes: self.width * 4,
                                    bounds: Rect::new(0, 0, self.width as i16,
                                                      self.height as i16),
                                    source_rect: call.source,
                                    destination_rect: call.destination,
                                    mode: call.mode,
                                    foreground: 0,
                                    background: 0xffffff,
                                    ..Default::default() },
            data))
    }

    pub(crate) fn bitmap(&mut self, call: &quickdraw::BitmapCall, data: &[u8], palette: &[u32],
                         mask: &[u8])
                         -> Result<(), Status>
    {
        let drawing = super::bitmaps_service::BitmapDrawing::new(call,
                                                                 data,
                                                                 palette,
                                                                 mask,
                                                                 self.port.clone())?;
        drawing.paint(&mut Canvas::new(&mut self.pixels, self.width, self.height))
    }

    pub fn graphics(&mut self, call: &mut quickdraw::GraphicsCall, points: &[Point], region: &[u8])
                    -> Result<(), Status>
    {
        let mut port = self.port.clone();
        if let Some(drawing) = service::graphics(&mut port, call, points, region)?
        {
            drawing.paint(&mut Canvas::new(&mut self.pixels, self.width, self.height))?;
        }
        self.port = port;
        Ok(())
    }

    pub fn text(&mut self, call: &mut quickdraw::FontCall, text: &str, fonts: &FontManager)
                -> Result<(), Status>
    {
        let mut canvas = Canvas::new(&mut self.pixels, self.width, self.height);
        let mut status = OK;
        service::with_clip(&mut canvas, &self.port, |canvas| {
            status = fonts_service::draw(fonts, call, text, canvas);
        });
        if status == OK { Ok(()) } else { Err(status) }
    }
}
