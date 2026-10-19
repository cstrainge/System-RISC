//! Resident offscreen ports. Handles never expose storage pointers to applications.
#![forbid(unsafe_code)]
use alloc::{vec, vec::Vec};

use mac_abi::*;

use super::{Canvas,
            fonts::FontManager,
            fonts_service,
            service::{self, Port}};
const FIRST: u32 = 0x80000000;
const MAX_PIXELS: usize = 512 * 1024;
pub struct World
{
    pub id:     u32,
    pub width:  usize,
    pub height: usize,
    pub pixels: Vec<u32>,
    pub port:   Port
}
#[derive(Default)]
pub struct Worlds
{
    next:   u32,
    worlds: Vec<World>
}
impl Worlds
{
    pub fn create(&mut self, width: i16, height: i16) -> Result<u32, Status>
    {
        if width <= 0 || height <= 0
        {
            return Err(BAD_ARGUMENT);
        }
        let size = width as usize * height as usize;
        if size > 640 * 480
           || self.worlds.iter().map(|w| w.pixels.len()).sum::<usize>() + size > MAX_PIXELS
        {
            return Err(NO_MEMORY);
        }
        self.next = self.next.checked_add(1).filter(|n| *n < FIRST).ok_or(NO_MEMORY)?;
        let id = self.next | FIRST;
        self.worlds.push(World { id,
                                 width: width as usize,
                                 height: height as usize,
                                 pixels: vec![0xffffff; size],
                                 port: Port::default() });
        Ok(id)
    }

    pub fn get(&self, id: u32) -> Result<&World, Status>
    {
        self.worlds.iter().find(|w| w.id == id).ok_or(NOT_FOUND)
    }

    pub fn get_mut(&mut self, id: u32) -> Result<&mut World, Status>
    {
        self.worlds.iter_mut().find(|w| w.id == id).ok_or(NOT_FOUND)
    }

    pub fn dispose(&mut self, id: u32) -> Result<(), Status>
    {
        self.get(id)?;
        self.worlds.retain(|w| w.id != id);
        Ok(())
    }

    pub fn is_world(id: u32) -> bool
    {
        id & FIRST != 0
    }
}
impl World
{
    pub fn graphics(&mut self, call: &mut quickdraw::GraphicsCall, points: &[Point], region: &[u8])
                    -> Result<(), Status>
    {
        if let Some(drawing) = service::graphics(&mut self.port, call, points, region)?
        {
            drawing.paint(&mut Canvas::new(&mut self.pixels, self.width, self.height))?;
        }
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
