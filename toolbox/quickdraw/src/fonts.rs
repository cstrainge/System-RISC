//! Font Manager queries and text drawing are resident System services.
use mac_abi::quickdraw::*;
pub use mac_abi::quickdraw::{BOLD, CONDENSE, EXTEND, FontMetrics, FontSelection, ITALIC, OUTLINE,
                             SHADOW, UNDERLINE};
pub use mac_traps::FontReply as Family;

use crate::{Client, Result};
mod suitcase;
pub use suitcase::{FontSuitcase, SuitcaseFamily};
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

    /// Catalog revision for detecting a changed available-family list.
    /// Changes when the set of selectable families changes.
    pub fn revision(self) -> Result<u32>
    {
        Ok(self.client.font(FONT_REVISION, FontSelection::default(), "", 0)?.index)
    }

    /// Persistent family identity, including a removed but still retained selection.
    /// Returns an opaque persistent key in the reply's name field.
    /// Existing selections retain their keys after source removal.
    pub fn key(self, family: i16) -> Result<Family>
    {
        self.client.font(FONT_KEY, FontSelection { family, ..Default::default() }, "", 0)
    }

    /// Resolve a persistent identity among currently available families.
    /// Resolves an available family; removed sources are not selectable.
    pub fn key_id(self, key: &str) -> Result<i16>
    {
        Ok(self.client.font(FONT_KEY_ID, FontSelection::default(), key, 0)?.selection.family)
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
