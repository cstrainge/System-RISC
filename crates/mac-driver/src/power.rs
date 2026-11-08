//! The power-class request engine every `POWR` provider shares.
//!
//! What a power request may be — its class, phase, operation and payload — is
//! the same question on every machine. Which mechanism carries the answer is
//! not. Keeping the first question here, once, is what stops two providers from
//! drifting into accepting different requests while both claim the same
//! interface; the second question belongs to each machine adapter.
#![forbid(unsafe_code)]

use mac_power_interface::{Action, CLASS};

use crate::{Completion, Error, Phase, Request, Result};

/// The machine mechanism behind one provider.
///
/// One entry point for both actions, so shutdown and restart are one mechanism
/// with a per-machine implementation rather than two call paths that could be
/// honoured differently.
pub trait Hardware
{
    fn trigger(&mut self, action: Action) -> Result<()>;
}

/// Validate one admitted request and hand it to the mechanism.
///
/// A completion means the trigger was issued. An action completes in its
/// invocation: a provider must not report Pending after touching hardware.
pub fn request(device: &mut impl Hardware, request: Request<'_>, output: &mut [u8])
               -> Result<Completion>
{
    if request.class != CLASS
    {
        return Err(Error(mac_abi::NOT_SUPPORTED));
    }
    if request.phase != Phase::Start || !request.input.is_empty() || !output.is_empty()
    {
        return Err(Error(mac_abi::BAD_ARGUMENT));
    }
    let action = Action::from_operation(request.operation).ok_or(Error(mac_abi::NOT_SUPPORTED))?;
    device.trigger(action)?;
    Ok(Completion::Complete(0))
}
