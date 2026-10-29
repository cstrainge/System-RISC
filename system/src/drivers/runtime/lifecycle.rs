//! Class-neutral scheduled module activation. The broker authenticates the
//! declaring module before this bounded leaf; no foreign callback runs here.
use mac_abi::Status;
use mac_module::{Offer, lifecycle as wire};

pub(super) fn install() -> Result<(), Status>
{
    super::register_synchronous_service(Offer { class:          wire::CLASS,
                                                version:        wire::VERSION,
                                                request_schema: 1,
                                                reply_schema:   1,
                                                capabilities:   0,
                                                max_request:    (4 + wire::MAX_CAUSE) as u32,
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
    super::RUNTIME.with(|runtime| {
                      runtime.registry
                             .startup_call(caller, operation, input, output)
                             .map_err(super::status)
                  })
                  .map_err(crate::state::AccessError::status)?
}
