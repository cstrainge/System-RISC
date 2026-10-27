//! Native installation and authenticated entry for the real resident SCEX store.
use mac_abi::Status;

use super::extension::{OFFER, Registration};

pub(crate) fn initialize() -> Result<(), Status>
{
    let kinds = crate::toolbox::handles::registry(|registry| {
                    let [a, b, c, d] =
                        Registration::ALL.map(|kind| registry.get(kind.specification().kind));
                    Ok::<_, mac_object_table::registry::Error>([a?, b?, c?, d?])
                }).map_err(|cause| cause.status())?
                  .map_err(|cause| crate::toolbox::handles::registry_status(&cause))?;
    crate::kernel::with_scrap(|manager| manager.prepare_extensions(kinds))
        .map_err(crate::state::AccessError::status)?.map_err(|cause| cause.status())?;
    crate::drivers::register_synchronous_service(OFFER, request, None)?;
    Ok(())
}
fn request(caller: u64, operation: u32, input: &[u8], output: &mut [u8]) -> Result<usize, Status>
{
    let authority = match crate::drivers::scrap_authority(caller)
    {
        Ok(Ok(authority)) => authority,
        Ok(Err(cause)) => return super::extension_wire::refuse(cause.status(), &cause, output),
        Err(cause) =>
        {
            return super::extension_wire::refuse(cause.status(),
                                                 &format_args!("SCEX authentication could not \
                                                                enter its resident module \
                                                                registry ({cause:?}); keep \
                                                                local Scrap and retry after \
                                                                the current operation"),
                                                 output);
        }
    };
    match crate::kernel::with_scrap(|manager| {
              super::extension_wire::dispatch(manager, &authority, operation, input, output)
          })
    {
        Ok(result) => result,
        Err(cause) =>
        {
            super::extension_wire::refuse(cause.status(),
                                          &format_args!("SCEX could not enter the resident Scrap \
                                                         Manager ({cause:?}); no clipboard \
                                                         operation ran, retain the original \
                                                         grant and retry"),
                                          output)
        }
    }
}
