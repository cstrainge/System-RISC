//! Icons resolve through the same immutable resource chain as pictures and fonts.
#![forbid(unsafe_code)]
use alloc::{collections::BTreeSet, sync::Arc, vec::Vec};

use mac_abi::{quickdraw::*,
              resources::{CHAIN, SYSTEM},
              *};
use mac_formats::resource::ResourceRef;

use super::{Canvas,
            icons::{self, IconFamily, IconFlags},
            service::{self, Port}};
use crate::toolbox::resources::manager::Manager;
/// Resolve local bundle IDs inside one resource file, never across app forks.
pub fn file_id(resources: &[mac_formats::resource::Resource], creator: [u8; 4], file_type: [u8; 4])
               -> Result<i16, Status>
{
    let refs: Vec<_> = resources.iter()
                                .map(|r| {
                                    ResourceRef { kind:       r.kind,
                                                  id:         r.id,
                                                  name:       r.name.as_deref(),
                                                  attributes: r.attributes,
                                                  data:       &r.data }
                                })
                                .collect();
    icons::file_icon_id(&refs, creator, file_type).map_err(status)
}
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
            let scopes = if call.scope == CHAIN
            {
                [mac_abi::resources::APPLICATION, SYSTEM]
            }
            else
            {
                [call.scope, call.scope]
            };
            for scope in scopes.into_iter().take(if call.scope == CHAIN { 2 } else { 1 })
            {
                // BNDL local IDs are meaningful only inside their own fork.
                // Never let another scope's FREF or icon shadow this mapping.
                let refs: Vec<_> = resources.iter()
                                            .filter(|(origin, _)| *origin == scope)
                                            .map(|(_, r)| {
                                                ResourceRef { kind:       r.kind,
                                                              id:         r.id,
                                                              name:       r.name.as_deref(),
                                                              attributes: r.attributes,
                                                              data:       &r.data }
                                            })
                                            .collect();
                match icons::file_icon_id(&refs, call.creator, call.file_type)
                {
                    Ok(id) =>
                    {
                        call.id = id;
                        call.origin = scope;
                        return Ok(());
                    }
                    Err(icons::IconError::NotFound) =>
                    {}
                    Err(error) => return Err(status(error))
                }
            }
            if ![SYSTEM, CHAIN].contains(&call.scope)
            {
                return Err(NOT_FOUND);
            }
            // The System's type bundles own generic driver/extension icons.
            // A third-party creator need not duplicate these resources; its
            // specific bundle above still wins. This remains resource based.
            let refs: Vec<_> = resources.iter()
                                        .filter(|(origin, _)| *origin == SYSTEM)
                                        .map(|(_, r)| {
                                            ResourceRef { kind:       r.kind,
                                                          id:         r.id,
                                                          name:       r.name.as_deref(),
                                                          attributes: r.attributes,
                                                          data:       &r.data }
                                        })
                                        .collect();
            call.id = icons::file_icon_id(&refs, *b"MACS", call.file_type).map_err(status)?;
            call.origin = SYSTEM;
            Ok(())
        }
        _ => Err(NOT_SUPPORTED)
    }
}
#[derive(Clone)]
pub struct Drawing
{
    pub family:      Arc<IconFamily>,
    pub destination: Rect,
    pub flags:       IconFlags,
    pub port:        Port
}
impl Drawing
{
    pub(crate) fn bytes(kind: [u8; 4], data: &[u8], destination: Rect) -> Result<Self, Status>
    {
        if !matches!(&kind,
                     b"ICON"
                     | b"ICN#"
                     | b"ics#"
                     | b"SICN"
                     | b"cicn"
                     | b"icl4"
                     | b"icl8"
                     | b"ics4"
                     | b"ics8")
        {
            return Err(NOT_SUPPORTED);
        }
        let family = IconFamily::from_lookup(0, |candidate, id| {
                         (id == 0 && candidate == kind).then_some(data)
                     }).map_err(status)?;
        Ok(Self { family: family.into(),
                  destination,
                  flags: IconFlags::default(),
                  port: Port::default() })
    }

    #[cfg(any(target_arch = "riscv64", feature = "test-support"))]
    pub(crate) fn file(files: &mut crate::toolbox::files::Manager, manager: Manager<'_>,
                       call: &FileIconCall)
                       -> Result<Self, Status>
    {
        if call.operation != ICON_PLOT_FILE || call.reserved != [0; 3]
        {
            return Err(BAD_ARGUMENT);
        }
        let flags = IconFlags::from_transform(call.flags).map_err(status)?;
        Ok(Self { family: files.file_icon(call.volume, call.cnid, manager)?.into(),
                  destination: call.destination,
                  flags,
                  port: Port::default() })
    }

    pub fn new(manager: Manager<'_>, call: &IconCall, port: Port) -> Result<Self, Status>
    {
        let flags = IconFlags::from_transform(call.flags).map_err(status)?;
        Ok(Self { family: family(manager, call.scope, call.id)?.into(),
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
