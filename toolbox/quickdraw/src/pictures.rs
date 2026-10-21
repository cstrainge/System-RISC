//! PICT parsing, font lookup and replay remain resident in System.
use crate::{Rect, Result, graphics::GraphicsPort};
impl GraphicsPort<'_>
{
    pub fn draw_picture(self, data: &[u8], destination: Rect) -> Result<()>
    {
        self.client.picture(self.window.0, data, destination)
    }

    pub fn picture_resource(self, scope: mac_traps::types::ResourceScope, id: i16,
                            destination: Rect)
                            -> Result<()>
    {
        self.client.picture_resource(self.window.0, scope.raw(), id, destination)
    }
}
impl crate::SurfacePort<'_>
{
    pub fn draw_picture(&mut self, data: &[u8], destination: Rect) -> Result<()>
    {
        self.connection.picture(data, destination)
    }
}
