//! Declared module preference service. Stable identity/authority come from the real
//! registry, never request bytes. The file transaction retains its own tickets.
use mac_abi::{BAD_ARGUMENT, CORRUPT_DATA, Status};
use mac_module::{Offer, preferences as wire};

pub(super) fn install() -> Result<(), Status>
{
    super::register_synchronous_service(Offer { class:          wire::CLASS,
                                                version:        wire::VERSION,
                                                request_schema: 1,
                                                reply_schema:   1,
                                                capabilities:   0,
                                                max_request:    wire::MAX_REQUEST as u32,
                                                max_reply:      wire::REPLY_BYTES as u32,
                                                flags:          Offer::EXCLUSIVE
                                                                | Offer::SERIALIZED
                                                                | Offer::KERNEL_ONLY },
                                        request,
                                        None)?;
    Ok(())
}
fn request(caller: u64, operation: u32, input: &[u8], output: &mut [u8]) -> Result<usize, Status>
{
    let (owner, identity) =
        super::RUNTIME.with(|runtime| {
                          runtime.registry.preference_subject(caller).map_err(super::status)
                      })
                      .map_err(crate::state::AccessError::status)??;
    let filename = wire::filename(identity);
    let name = core::str::from_utf8(&filename).map_err(|_| CORRUPT_DATA)?;
    if operation == wire::REVISION && input.is_empty()
    {
        if output.len() < wire::REPLY_BYTES
        {
            return Err(BAD_ARGUMENT);
        }
        let revision = crate::kernel::with_files(|files| files.module_preference_revision())
            .map_err(crate::state::AccessError::status)??;
        output[..8].copy_from_slice(&revision.to_le_bytes());
        return Ok(8);
    }
    let (expected, new) = match operation
    {
        wire::LOAD if input.is_empty() => (None, &[][..]),
        wire::COMPARE_EXCHANGE if input.len() >= 4 =>
        {
            let length = u32::from_le_bytes(input[..4].try_into().map_err(|_| BAD_ARGUMENT)?);
            let (expected, new) = if length == wire::ABSENT
            {
                (None, &input[4..])
            }
            else
            {
                let length = length as usize;
                if length > wire::MAX_BYTES || input.len() < 4 + length
                {
                    return Err(BAD_ARGUMENT);
                }
                (Some(&input[4..4 + length]), &input[4 + length..])
            };
            #[derive(serde::Deserialize)]
            struct Version
            {
                version: u32
            }
            let (version, used): (Version, usize) =
                serde_json_core::from_slice(new).map_err(|_| CORRUPT_DATA)?;
            if version.version == 0
               || !new[used..].iter().all(u8::is_ascii_whitespace)
               || new.len() > wire::MAX_BYTES
            {
                return Err(CORRUPT_DATA);
            }
            (Some(expected), new)
        }
        _ => return Err(BAD_ARGUMENT)
    };
    crate::kernel::with_files(|files| files.module_preferences(&owner, name, expected, new, output))
        .map_err(crate::state::AccessError::status)?
}
