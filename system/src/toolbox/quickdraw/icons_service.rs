//! Icons resolve through the same immutable resource chain as pictures and fonts.
#![forbid(unsafe_code)]
use alloc::{collections::BTreeSet, vec::Vec};

use mac_abi::{quickdraw::*,
              resources::{CHAIN, SYSTEM},
              *};
use mac_formats::resource::ResourceRef;

use super::{Canvas,
            icons::{self, IconFamily, IconFlags},
            service::{self, Port}};
use crate::toolbox::resources::manager::Manager;
pub fn status(error: icons::IconError) -> Status
{
    match error
    {
        icons::IconError::NotFound => NOT_FOUND,
        icons::IconError::Unsupported => NOT_SUPPORTED,
        icons::IconError::TooLarge => NO_MEMORY,
        _ => BAD_ARGUMENT
    }
}
pub fn family(manager: Manager<'_>, scope: i16, id: i16) -> Result<IconFamily, Status>
{
    IconFamily::from_lookup(id, |kind, rid| {
        manager.get(if kind == *b"clut" { SYSTEM } else { scope },
                    ResourceKey { kind,
                                  id: rid })
               .ok()
               .map(|(_, r)| r.data.as_slice())
    }).map_err(status)
}
pub fn query(manager: Manager<'_>, call: &mut IconCall) -> Result<(), Status>
{
    let resources = manager.all(call.scope)?;
    match call.operation
    {
        ICON_COUNT | ICON_AT =>
        {
            let mut seen = BTreeSet::new();
            let ids: Vec<_> =
                resources.iter()
                         .filter_map(|(_, r)| {
                             (matches!(&r.kind, b"ICN#" | b"ics#" | b"ICON" | b"SICN" | b"cicn")
                              && seen.insert(r.id)).then_some(r.id)
                         })
                         .collect();
            if call.operation == ICON_COUNT
            {
                call.index = ids.len() as u32;
            }
            else
            {
                call.id = *ids.get(call.index as usize).ok_or(NOT_FOUND)?;
            }
            Ok(())
        }
        ICON_FILE =>
        {
            let refs: Vec<_> = resources.iter()
                                        .map(|(_, r)| {
                                            ResourceRef { kind:       r.kind,
                                                          id:         r.id,
                                                          name:       r.name.as_deref(),
                                                          attributes: r.attributes,
                                                          data:       &r.data }
                                        })
                                        .collect();
            call.id = icons::file_icon_id(&refs, call.creator, call.file_type).map_err(status)?;
            call.origin =
                resources.iter()
                         .find(|(_, r)| r.kind == *b"BNDL" && r.data.starts_with(&call.creator))
                         .map_or(CHAIN, |(origin, _)| *origin);
            Ok(())
        }
        _ => Err(NOT_SUPPORTED)
    }
}
#[derive(Clone)]
pub struct Drawing
{
    pub family:      IconFamily,
    pub destination: Rect,
    pub flags:       IconFlags,
    pub port:        Port
}
impl Drawing
{
    pub fn new(manager: Manager<'_>, call: &IconCall, port: Port) -> Result<Self, Status>
    {
        let flags = IconFlags::from_transform(call.flags).map_err(status)?;
        Ok(Self { family: family(manager, call.scope, call.id)?,
                  destination: call.destination,
                  flags,
                  port })
    }

    pub fn storage(&self) -> usize
    {
        self.port.storage()
        + self.family.images.iter().map(|i| i.pixels.len() * 4 + i.mask.len()).sum::<usize>()
    }

    pub fn paint(&self, canvas: &mut Canvas<'_>) -> Result<(), Status>
    {
        let mut result = Ok(());
        service::with_clip(canvas, &self.port, |canvas| {
            result = self.family.draw(canvas, self.destination, self.flags).map_err(status);
        });
        result
    }
}
