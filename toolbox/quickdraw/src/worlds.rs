//! Owned offscreen drawing storage lives in System and is released by Drop.
use mac_abi::quickdraw::*;

use crate::{Client, Point, Rect, Result,
            graphics::{GraphicsPort, TransferMode}};
pub struct GWorld<'a>
{
    client: Client<'a>,
    id:     u64
}
impl<'a> GWorld<'a>
{
    pub fn new(client: Client<'a>, width: i16, height: i16) -> Result<Self>
    {
        let call = client.world(WORLD_NEW,
                                0,
                                Point { h: width,
                                        v: height },
                                &mut [])?;
        Ok(Self { client,
                  id: call.world })
    }

    pub fn graphics(&self) -> GraphicsPort<'a>
    {
        GraphicsPort::new(self.client, DrawingTarget::World(WorldId(self.id)))
    }

    pub fn dimensions(&self) -> Result<Point>
    {
        Ok(self.client.world(WORLD_INFO, self.id, Point::default(), &mut [])?.dimensions)
    }

    pub fn pixels(&self, output: &mut [u32]) -> Result<usize>
    {
        Ok(self.client.world(WORLD_READ, self.id, Point::default(), output)?.pixel_length)
    }

    pub fn set_pixels(&self, input: &[u32]) -> Result<()>
    {
        let size = self.dimensions()?;
        let bounds = Rect::new(0, 0, size.h, size.v);
        self.graphics().copy_bits(&crate::bitmaps::Bitmap::Rgb32 { pixels: input,
                                                                   row_pixels: size.h as usize,
                                                                   bounds },
                                  bounds,
                                  bounds,
                                  TransferMode::Copy,
                                  &[])
    }

    pub fn copy_to(&self, target: GraphicsPort<'_>, source: Rect, destination: Rect,
                   mode: TransferMode)
                   -> Result<()>
    {
        self.client.world_copy(self.id, target.target, source, destination, mode as i16)
    }
}
impl Drop for GWorld<'_>
{
    fn drop(&mut self)
    {
        let _ = self.client.world(WORLD_DISPOSE, self.id, Point::default(), &mut []);
    }
}
impl<'a> crate::QuickDraw<'a>
{
    pub fn new_gworld(self, width: i16, height: i16) -> Result<GWorld<'a>>
    {
        GWorld::new(self.client, width, height)
    }
}
