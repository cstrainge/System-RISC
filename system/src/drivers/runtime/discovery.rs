//! Boot-only class-neutral module ordering and hardware admission.
#![forbid(unsafe_code)]
use super::*;

fn offers(metadata: &[u8], dependency: mac_module::Dependency) -> bool
{
    mac_module::Contract::decode(metadata).is_ok_and(|contract| {
                                              (dependency.module == [0; 16]
                                               || dependency.module == contract.identity)
                                              && contract.offers().any(|offer| {
                                                                      offer.is_ok_and(|offer| {
                                                                               offer.class
                                                                          == dependency.class
                                                                          && offer.version
                                                                             == dependency.version
                                                                           })
                                                                  })
                                          })
}

pub(crate) fn discover(boot: &mut crate::boot_screen::BootScreen) -> Result<(), Status>
{
    let candidates=crate::kernel::with_files(|files|files.module_candidates()).map_err(crate::state::AccessError::status)??;
    if candidates.is_empty()
    {
        return Ok(());
    }
    let mut waiting = Vec::new();
    let mut absent: Vec<Vec<u8>> = Vec::new();
    let mut needs_devices = false;
    waiting.try_reserve_exact(candidates.len()).map_err(|_| NO_MEMORY)?;
    absent.try_reserve_exact(candidates.len()).map_err(|_| NO_MEMORY)?;
    for candidate in candidates
    {
        if candidate.file_type != candidate.directory.file_type()
        {
            serial!("MODULE_REJECTED name={} reason=wrong-installation-directory",
                    core::str::from_utf8(&candidate.name).unwrap_or("?"));
            if candidate.file_type == *b"INIT"
            {
                boot.extension_refused(core::str::from_utf8(&candidate.name).unwrap_or("?"),
                                       candidate.volume,
                                       candidate.cnid,
                                       crate::boot_screen::ExtensionRefusal::Location,
                                       BAD_ARGUMENT);
            }
            continue;
        }
        let metadata = crate::kernel::with_files(|files| {
                           files.module_metadata(candidate.volume, candidate.cnid)
                       }).map_err(crate::state::AccessError::status)?;
        let metadata =
            metadata.and_then(|metadata| {
                        let contract =
                            mac_module::Contract::decode(&metadata).map_err(|_| BAD_ABI)?;
                        needs_devices |= contract.devices().next().is_some();
                        Ok(metadata)
                    });
        match metadata
        {
            Ok(metadata) => waiting.push((candidate, metadata)),
            Err(status) =>
            {
                let failure = admission::metadata_refusal(status);
                serial!("MODULE_REJECTED name={} reason={} source_status={} status={} next={}",
                        core::str::from_utf8(&candidate.name).unwrap_or("?"),
                        failure.reason,
                        status,
                        failure.status,
                        failure.action);
                if candidate.directory == PackageKind::Extension
                {
                    boot.extension_refused(core::str::from_utf8(&candidate.name).unwrap_or("?"),
                                           candidate.volume,
                                           candidate.cnid,
                                           failure.refusal,
                                           failure.status);
                }
            }
        }
    }
    let inventory = if needs_devices
    {
        crate::machine::driver_devices::inventory()?
    }
    else
    {
        [None; crate::machine::driver_devices::DEVICE_SLOTS]
    };
    while !waiting.is_empty()
    {
        let mut progress = false;
        let mut index = 0;
        while index < waiting.len()
        {
            let (candidate, metadata) = &waiting[index];
            let contract = mac_module::Contract::decode(metadata).map_err(|_| BAD_ABI)?;
            let admitted = RUNTIME.with(|runtime| {
                                      runtime.registry.admit(contract, candidate.directory,
                                                             candidate.file_type)
                                  })
                                  .map_err(crate::state::AccessError::status)?;
            if admitted == Err(Error::Contract(mac_module::Error::MissingDependency))
            {
                // A wildcard can still use another ready or waiting provider.
                // Propagate only a missing dependency whose remaining providers
                // are known to be absent, never a generic missing/cyclic edge.
                let unavailable = RUNTIME.with(|runtime| {
                                             contract.dependencies().any(|dependency| {
                                                                        dependency.is_ok_and(|d| {
                        !d.optional && !runtime.registry.dependency_available(d)
                        && absent.iter().any(|m| offers(m,d))
                        && !waiting.iter().any(|(_,m)| offers(m,d))
                    })
                                                                    })
                                         })
                                         .map_err(crate::state::AccessError::status)?;
                if unavailable
                {
                    let (candidate, metadata) = waiting.remove(index);
                    serial!("MODULE_SKIPPED name={} reason=dependency-device-absent",
                            core::str::from_utf8(&candidate.name).unwrap_or("?"));
                    if candidate.directory == PackageKind::Extension
                    {
                        boot.extension_refused(core::str::from_utf8(&candidate.name).unwrap_or("?"),
                            candidate.volume, candidate.cnid, crate::boot_screen::ExtensionRefusal::Dependency, NOT_FOUND);
                    }
                    absent.push(metadata);
                    progress = true;
                    continue;
                }
                index += 1;
                continue;
            }
            let mut declared = false;
            let mut matched = false;
            if admitted.is_ok()
            {
                for matcher in contract.devices()
                {
                    let matcher = matcher.map_err(|_| BAD_ABI)?;
                    declared = true;
                    if inventory.iter().flatten().any(|d| matcher.matches(d.bus, d.key))
                    {
                        matched = true;
                        break;
                    }
                }
            }
            let (candidate, metadata) = waiting.remove(index);
            progress = true;
            if let Err(error) = admitted
            {
                serial!("MODULE_REJECTED name={} reason={:?}",
                        core::str::from_utf8(&candidate.name).unwrap_or("?"),
                        error);
                if candidate.directory == PackageKind::Extension
                {
                    let cause = match error
                    {
                        Error::Contract(mac_module::Error::WrongLocation) =>
                        {
                            crate::boot_screen::ExtensionRefusal::Location
                        }
                        Error::Contract(mac_module::Error::MissingDependency
                                        | mac_module::Error::MissingService) =>
                        {
                            crate::boot_screen::ExtensionRefusal::Dependency
                        }
                        Error::Contract(_) => crate::boot_screen::ExtensionRefusal::Contract,
                        _ => crate::boot_screen::ExtensionRefusal::Initialization(status(error))
                    };
                    boot.extension_refused(core::str::from_utf8(&candidate.name).unwrap_or("?"),
                                           candidate.volume,
                                           candidate.cnid,
                                           cause,
                                           status(error));
                }
                continue;
            }
            if declared && !matched
            {
                serial!("MODULE_SKIPPED name={} reason=device-absent",
                        core::str::from_utf8(&candidate.name).unwrap_or("?"));
                if candidate.directory == PackageKind::Extension
                {
                    boot.extension_refused(core::str::from_utf8(&candidate.name).unwrap_or("?"),
                                           candidate.volume,
                                           candidate.cnid,
                                           crate::boot_screen::ExtensionRefusal::Device,
                                           NOT_FOUND);
                }
                absent.push(metadata);
                continue;
            }
            let resources = crate::kernel::with_files(|files| {
                                files.module_resources(candidate.volume, candidate.cnid)
                            }).map_err(crate::state::AccessError::status)?;
            let result = if candidate.directory == PackageKind::Extension
            {
                boot.extension_attempt(core::str::from_utf8(&candidate.name).unwrap_or("?"),
                                       candidate.volume,
                                       candidate.cnid,
                                       None,
                                       || {
                                           resources.and_then(|resources| {
                                                        load(&resources, candidate.file_type,
                                                             candidate.directory)
                                                    })
                                       })
            }
            else
            {
                resources.and_then(|resources| {
                             load(&resources, candidate.file_type, candidate.directory)
                         })
            };
            if let Err(status) = result
            {
                serial!("MODULE_REJECTED name={} status={}",
                        core::str::from_utf8(&candidate.name).unwrap_or("?"),
                        status);
            }
        }
        if !progress
        {
            for (candidate, _) in waiting
            {
                serial!("MODULE_REJECTED name={} reason=missing-or-cyclic-dependency",
                        core::str::from_utf8(&candidate.name).unwrap_or("?"));
                if candidate.directory == PackageKind::Extension
                {
                    boot.extension_refused(core::str::from_utf8(&candidate.name).unwrap_or("?"),
                                           candidate.volume,
                                           candidate.cnid,
                                           crate::boot_screen::ExtensionRefusal::Cycle,
                                           BAD_ABI);
                }
            }
            break;
        }
    }
    Ok(())
}
