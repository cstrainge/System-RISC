//! Class adapter for declarative file actions and local icon families. The
//! generic registry owns publication/lifetime; this adapter knows FSYS semantics.
#![forbid(unsafe_code)]
use alloc::{sync::Arc, vec::Vec};

use mac_filesystem_interface as fs;
use mac_module::{Contract,
                 file_handlers::{Action, Rule, Rules}};

use super::*;
use crate::toolbox::quickdraw::{icons::IconFamily, icons_service};
pub(super) struct Icon
{
    pub rule: Rule,
    family:   Option<Arc<IconFamily>>
}
pub(super) fn prepare(resources: &[Resource], contract: Contract<'_>) -> Result<Vec<Icon>, Status>
{
    let mut associations = resources.iter().filter(|r| r.kind == *b"FASS");
    let Some(resource) = associations.next()
    else
    {
        return Ok(Vec::new());
    };
    if resource.id != 0 || associations.next().is_some()
    {
        return Err(BAD_ABI);
    }
    let rules = Rules::decode(&resource.data).map_err(|_| BAD_ABI)?;
    let mut handlers = Vec::new();
    handlers.try_reserve_exact(rules.count()).map_err(|_| NO_MEMORY)?;
    for i in 0..rules.count()
    {
        let rule = rules.at(i).map_err(|_| BAD_ABI)?;
        match rule.action
        {
            Action::MountFilesystem =>
            {
                if contract.kind != PackageKind::Driver
                   || !contract.offers().any(|o| {
                                            o.is_ok_and(|o| {
                                                 o.class.0 == fs::CLASS
                                                 && o.version == fs::VERSION
                                                 && o.capabilities & fs::SOURCE_FILE != 0
                                             })
                                        })
                {
                    return Err(BAD_ABI);
                }
            }
        }
        let family = if rule.icon == 0
        {
            None
        }
        else
        {
            let resident = crate::kernel::resources().ok_or(NOT_SUPPORTED)?;
            Some(Arc::new(IconFamily::from_lookup(rule.icon, |kind, id| {
                              if kind == *b"clut"
                              {
                                  resident.get(mac_abi::resources::SYSTEM,
                                               mac_abi::ResourceKey { kind,
                                                                      id })
                                          .ok()
                                          .map(|(_, r)| r.data.as_slice())
                              }
                              else
                              {
                                  resources.iter()
                                           .find(|r| r.kind == kind && r.id == id)
                                           .map(|r| r.data.as_slice())
                              }
                          }).map_err(icons_service::status)?))
        };
        handlers.push(Icon { rule,
                             family });
    }
    Ok(handlers)
}
pub(crate) fn file_handler_provider(file_type: [u8; 4], name: &str) -> Result<Option<u32>, Status>
{
    RUNTIME.with(|runtime| {
               let Some((module, rule)) = runtime.registry.file_handler(file_type, name)
               else
               {
                   return Ok(None);
               };
               match rule.action
               {
                   Action::MountFilesystem =>
                   {
                       runtime.registry
                              .find(ClassId(fs::CLASS), fs::VERSION)
                              .find(|(_, i)| {
                                  i.module == module && i.offer.capabilities & fs::SOURCE_FILE != 0
                              })
                              .map(|(id, _)| Some(id))
                              .ok_or(BAD_ABI)
                   }
               }
           })
           .map_err(crate::state::AccessError::status)?
}
pub(crate) fn file_handler_icon(file_type: [u8; 4], name: &str)
                                -> Result<Option<Arc<IconFamily>>, Status>
{
    RUNTIME.with(|runtime| {
               let Some((module, rule)) = runtime.registry.file_handler(file_type, name)
               else
               {
                   return Ok(None);
               };
               let binding = runtime.bindings.iter().find(|b| b.module == module).ok_or(BAD_ABI)?;
               let handler =
                   binding.file_handlers.iter().find(|h| h.rule == rule).ok_or(BAD_ABI)?;
               Ok(handler.family.clone())
           })
           .map_err(crate::state::AccessError::status)?
}
