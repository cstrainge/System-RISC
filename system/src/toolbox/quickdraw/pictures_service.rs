#![forbid(unsafe_code)]
use mac_abi::*;

use super::{Canvas,
            fonts::FontManager,
            picture::{self, Picture},
            service::{self, Port}};
pub fn status(error: picture::Error) -> Status
{
    match error
    {
        picture::Error::UnsupportedOpcode(_)
        | picture::Error::UnsupportedMode(_)
        | picture::Error::UnsupportedState => NOT_SUPPORTED,
        picture::Error::FontUnavailable => NOT_FOUND,
        picture::Error::TooLarge => NO_MEMORY,
        _ => BAD_ARGUMENT
    }
}
#[derive(Clone)]
pub struct Drawing
{
    pub picture:     Picture,
    pub destination: Rect,
    pub port:        Port,
    pub bytes:       usize
}
impl Drawing
{
    pub fn new(data: &[u8], destination: Rect, port: Port, fonts: &FontManager)
               -> Result<Self, Status>
    {
        let picture = Picture::parse(data).map_err(status)?;
        picture.validate_with_fonts(fonts).map_err(status)?;
        picture.validate_destination(destination).map_err(status)?;
        Ok(Self { picture,
                  destination,
                  port,
                  bytes: data.len() })
    }

    pub fn storage(&self) -> usize
    {
        self.bytes + self.port.storage() + self.picture.commands.len() * 128
    }

    pub fn paint(&self, canvas: &mut Canvas<'_>, fonts: &FontManager) -> Result<(), Status>
    {
        let mut result = Ok(());
        service::with_clip(canvas, &self.port, |canvas| {
            result = self.picture.replay(canvas, fonts, self.destination).map_err(status);
        });
        result
    }
}
