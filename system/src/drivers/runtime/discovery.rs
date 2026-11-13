//! Boot-only module ordering and hardware admission. Ordering knows one
//! class: a module that offers the display goes first, and System attaches to
//! it before entering any other module, so the boot screen is up while the
//! rest of the machine is found.
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

/// Whether this module's contract offers the display class.
fn shows(metadata: &[u8]) -> bool
{
    mac_module::Contract::decode(metadata).is_ok_and(|contract| {
        contract.offers()
                .any(|offer| offer.is_ok_and(|offer| offer.class == mac_video_interface::CLASS))
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
    // Display providers first, each group otherwise in its installed order.
    let mut front = 0;
    for index in 0..waiting.len()
    {
        if shows(&waiting[index].1)
        {
            waiting[front..=index].rotate_right(1);
            front += 1;
        }
    }
    let inventory =
        if needs_devices { crate::machine::driver_devices::inventory()? } else { Vec::new() };
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
                                                        load(resources, candidate.file_type,
                                                             candidate.directory)
                                                    })
                                       })
            }
            else
            {
                resources.and_then(|resources| {
                             load(resources, candidate.file_type, candidate.directory)
                         })
            };
            if let Err(status) = result
            {
                serial!("MODULE_REJECTED name={} status={}",
                        core::str::from_utf8(&candidate.name).unwrap_or("?"),
                        status);
            }
            else if shows(&metadata) && !crate::machine::video::attached()
            {
                // The boot screen has been drawn into System's own reservation
                // until now; attaching carries it onto the real display. A
                // refusal here is not final: boot attaches again after
                // discovery and decides there whether a display is required.
                match crate::machine::video::attach()
                {
                    Ok(true) =>
                    {
                        serial!("BOOT_DISPLAY_FIRST name={}: attached before the remaining \
                                 modules",
                                core::str::from_utf8(&candidate.name).unwrap_or("?"))
                    }
                    Ok(false) => (),
                    Err(status) => serial!("BOOT_DISPLAY_DEFERRED status={status}")
                }
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
