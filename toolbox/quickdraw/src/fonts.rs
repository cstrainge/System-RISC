//! Font Manager queries and text drawing are resident System services.
use mac_abi::quickdraw::*;
pub use mac_abi::quickdraw::{BOLD, CONDENSE, EXTEND, FontMetrics, FontSelection, ITALIC, OUTLINE,
                             SHADOW, UNDERLINE};
pub use mac_traps::FontReply as Family;

use crate::{Client, Result};
#[derive(Clone, Copy)]
pub struct Fonts<'a>
{
    client: Client<'a>
}
impl<'a> Fonts<'a>
{
    pub const fn new(client: Client<'a>) -> Self
    {
        Self { client }
    }

    pub fn count(self) -> Result<u32>
    {
        Ok(self.client.font(FONT_COUNT, FontSelection::default(), "", 0)?.index)
    }

    pub fn family(self, index: u32) -> Result<Family>
    {
        self.client.font(FONT_AT, FontSelection::default(), "", index)
    }

    pub fn name(self, family: i16) -> Result<Family>
    {
        self.client.font(FONT_NAME,
                         FontSelection { family,
                                         ..Default::default() },
                         "",
                         0)
    }

    pub fn id(self, name: &str) -> Result<i16>
    {
        Ok(self.client.font(FONT_ID, FontSelection::default(), name, 0)?.selection.family)
    }

    pub fn metrics(self, font: FontSelection) -> Result<FontMetrics>
    {
        Ok(self.client.font(FONT_METRICS, font, "", 0)?.metrics)
    }

    pub fn text_width(self, font: FontSelection, text: &str) -> Result<i16>
    {
        Ok(self.client.font(FONT_WIDTH, font, text, 0)?.width)
    }
}
impl<'a> crate::QuickDraw<'a>
{
    pub fn fonts(self) -> Fonts<'a>
    {
        Fonts::new(self.client)
    }
}
