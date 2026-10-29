//! Typed scheduler preparation/control. All authority is resident-issued; a
//! missing SCTL service refuses rather than simulating takeover or CPU release.
use core::fmt;

use mac_abi::{BAD_ABI, BAD_ARGUMENT, NOT_SUPPORTED};
use mac_scheduler_interface::{self as wire,
                              control::{self, Limits, Registration, State}};

use crate::{Client, Driver, Error, Provider, Result};
#[derive(Clone, Debug)]
pub struct ControlError
{
    status:    i32,
    operation: u32,
    cause:     [u8; control::MAX_CAUSE],
    length:    usize
}
impl ControlError
{
    fn new(operation: u32, status: i32, text: &str) -> Self
    {
        let mut length = text.len().min(control::MAX_CAUSE);
        while !text.is_char_boundary(length)
        {
            length -= 1;
        }
        let mut cause = [0; control::MAX_CAUSE];
        cause[..length].copy_from_slice(&text.as_bytes()[..length]);
        Self { status,
               operation,
               cause,
               length }
    }

    pub fn cause(&self) -> &str
    {
        core::str::from_utf8(&self.cause[..self.length]).unwrap_or("SCTL diagnostic UTF-8 \
                                                                    invariant failed")
    }

    pub fn status(&self) -> i32
    {
        self.status
    }
}
impl fmt::Display for ControlError
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        let text = core::str::from_utf8(&self.cause[..self.length]).map_err(|_| fmt::Error)?;
        write!(f, "SCTL operation {} refused ({}): {text}", self.operation, self.status)
    }
}
impl core::error::Error for ControlError
{
}
pub type ControlResult<T> = core::result::Result<T, ControlError>;
/// Non-copyable original-provider controller. Explicit retirement preserves
/// this controller on refusal. Drop makes one bounded retirement attempt;
/// resident owner cleanup/quarantine still owns every unacknowledged grant.
pub struct Controller
{
    provider:   Provider,
    generation: Option<u64>,
    uncertain:  bool,
    limits:     Option<Limits>
}
impl Controller
{
    pub fn new(client: Client) -> ControlResult<Self>
    {
        let provider=client.resolve(wire::CONTROL_CLASS,wire::VERSION).map_err(|e|ControlError::new(0,e.0,"declare the actual resident SCTL v1 prerequisite; bootstrap remains active"))?;
        Ok(Self { provider,
                  generation: None,
                  uncertain: false,
                  limits: None })
    }

    fn call(&self, operation: u32, input: &[u8], output: &mut [u8; control::MAX_REPLY])
            -> ControlResult<usize>
    {
        let length=self.provider.synchronous(operation,input,output).map_err(|e|ControlError::new(operation,e.0,"resident call refused; retain the original registration/owner for retry or cleanup"))?;
        match control::Reply::decode(&output[..length]).map_err(|_| {
                                                           ControlError::new(operation,
                                                                             BAD_ABI,
                                                                             "malformed reply; \
                                                                              preparation may \
                                                                              have an effect; do \
                                                                              not repeat it")
                                                       })?
        {
            control::Reply::Success(payload) => Ok(payload.len()),
            control::Reply::Failure { status,
                                      cause } => Err(ControlError::new(operation, status, cause))
        }
    }

    pub fn limits(&mut self) -> ControlResult<Limits>
    {
        let mut output = [0; control::MAX_REPLY];
        let length = self.call(control::LIMITS, &[], &mut output)?;
        let limits = Limits::decode(&output[8..8 + length]).map_err(|_| {
                                                               ControlError::new(control::LIMITS,
                                           BAD_ABI,
                                           "invalid native scheduler limits; no takeover is \
                                            permitted")
                                                           })?;
        self.limits = Some(limits);
        Ok(limits)
    }

    fn accept(&mut self, operation: u32, bytes: &[u8]) -> ControlResult<Registration>
    {
        let registration = Registration::decode(bytes).map_err(|_| {
                                                          ControlError::new(operation,
                                                 BAD_ABI,
                                                 "invalid registration; retain the original owner \
                                                  and generation")
                                                      })?;
        if let Some(generation) = self.generation
        {
            if registration.generation != generation
               && !(operation == control::RETIRE
                    && registration.state == State::Retired
                    && registration.generation == 0)
            {
                return Err(ControlError::new(operation,
                                             BAD_ABI,
                                             "registration generation changed; original grant \
                                              remains retained"));
            }
        }
        else if registration.generation != 0
        {
            self.generation = Some(registration.generation);
        }
        if self.limits.is_some_and(|l| registration.active_harts > l.maximum_active_harts)
        {
            return Err(ControlError::new(operation, BAD_ABI, "impossible active-hart count"));
        }
        Ok(registration)
    }

    pub fn prepare(&mut self, features: u32) -> ControlResult<Registration>
    {
        if self.generation.is_some() || self.uncertain
        {
            return Err(ControlError::new(control::PREPARE,
                                         BAD_ARGUMENT,
                                         "preparation already owns or may own a grant; retire \
                                          it before retry"));
        }
        let limits = self.limits.ok_or_else(|| {
                                     ControlError::new(control::PREPARE,
                                                       BAD_ARGUMENT,
                                                       "obtain genuine limits before preparing \
                                                        policy")
                                 })?;
        let requirements = control::Requirements { features,
                                                   candidate_capacity: limits.maximum_candidates,
                                                   topology_epoch: limits.topology_epoch };
        requirements.validate(limits).map_err(|_| {
                                          ControlError::new(control::PREPARE,
                                                            NOT_SUPPORTED,
                                                            "requested mechanisms are \
                                                             unavailable; bootstrap remains active")
                                      })?;
        self.uncertain = true;
        let mut output = [0; control::MAX_REPLY];
        let length = self.call(control::PREPARE, &requirements.encode(), &mut output)?;
        let registration = self.accept(control::PREPARE, &output[8..8 + length])?;
        if registration.state != State::Prepared || registration.generation == 0
        {
            return Err(ControlError::new(control::PREPARE,
                                         BAD_ABI,
                                         "prepare did not acknowledge a real prepared \
                                          registration"));
        }
        self.uncertain = false;
        Ok(registration)
    }

    fn known(&self, operation: u32) -> ControlResult<u64>
    {
        self.generation.ok_or_else(|| {
                           ControlError::new(operation,
                                             BAD_ARGUMENT,
                                             "no acknowledged registration; retain uncertain \
                                              owner cleanup instead")
                       })
    }

    fn operation(&mut self, operation: u32, input: &[u8]) -> ControlResult<Registration>
    {
        let mut output = [0; control::MAX_REPLY];
        let length = self.call(operation, input, &mut output)?;
        self.accept(operation, &output[8..8 + length])
    }

    /// Records the package's explicit ascending-hardware-ID activation intent.
    /// An acknowledged request is not completed secondary admission.
    pub fn select_harts(&mut self, count: u32) -> ControlResult<Registration>
    {
        let generation = self.known(control::HARTS)?;
        let limits =
            self.limits
                .ok_or_else(|| ControlError::new(control::HARTS, BAD_ARGUMENT, "missing limits"))?;
        if count > limits.maximum_active_harts
           || count > 1 && limits.features & control::MULTI_HART == 0
        {
            return Err(ControlError::new(control::HARTS,
                                         NOT_SUPPORTED,
                                         "secondary dispatch mechanism is unavailable; do not \
                                          release peers"));
        }
        let input = control::HartRequest { generation,
                                           count }.encode()
                                                  .map_err(|_| {
                                                      ControlError::new(control::HARTS,
                                                                        BAD_ARGUMENT,
                                                                        "invalid hart selection")
                                                  })?;
        self.operation(control::HARTS, &input)
    }

    /// Arms the request. Actual shadow validation, policy change and MLIF
    /// confirmation occur later from a genuine core boundary, never here.
    pub fn arm(&mut self) -> ControlResult<Registration>
    {
        let generation = self.known(control::ARM)?;
        let state = self.operation(control::ARM, &generation.to_le_bytes())?;
        if state.state != State::Armed
        {
            return Err(ControlError::new(control::ARM,
                                         BAD_ABI,
                                         "arm reply did not retain the pending handover"));
        }
        Ok(state)
    }

    pub fn status(&mut self) -> ControlResult<Registration>
    {
        let generation = self.known(control::STATUS)?;
        self.operation(control::STATUS, &generation.to_le_bytes())
    }

    pub fn retire(&mut self) -> ControlResult<bool>
    {
        if self.generation.is_none() && !self.uncertain
        {
            return Ok(true);
        }
        let generation = self.generation.unwrap_or(0);
        let state = self.operation(control::RETIRE, &generation.to_le_bytes())?;
        match state.state
        {
            State::Retired =>
            {
                self.generation = None;
                self.uncertain = false;
                Ok(true)
            }
            State::Retiring | State::Quarantined => Ok(false),
            _ =>
            {
                Err(ControlError::new(control::RETIRE,
                                      BAD_ABI,
                                      "retirement has no quiescent acknowledgement; retain \
                                       original grant"))
            }
        }
    }
}
impl Drop for Controller
{
    fn drop(&mut self)
    {
        if self.generation.is_none() && !self.uncertain
        {
            return;
        }
        match self.retire()
        {
            Ok(true) => (),
            Ok(false) =>
            {
                crate::runtime::diagnostic(format_args!("SCHEDULER_RETIRE_RETAINED module={} \
                                                         generation={:?} callbacks/harts not \
                                                         quiescent; resident owner cleanup \
                                                         retained\n",
                                                        self.provider.client.module(),
                                                        self.generation))
            }
            Err(cause) =>
            {
                crate::runtime::diagnostic(format_args!("SCHEDULER_RETIRE_RETAINED module={} \
                                                         generation={:?} cause={cause}; resident \
                                                         owner cleanup retained\n",
                                                        self.provider.client.module(),
                                                        self.generation))
            }
        }
    }
}
/// Private synchronous policy branch of the single audited module dispatch
/// boundary. Core still enforces execution restrictions and live commit checks.
pub(crate) fn dispatch(driver: &impl Driver, round: u64, input: &[u8], output: &mut [u8])
                       -> Result<usize>
{
    let view = wire::Round::decode(input).map_err(|_| Error(BAD_ABI))?;
    if view.context().round != round || output.len() < wire::DECISION_BYTES
    {
        return Err(Error(BAD_ARGUMENT));
    }
    let decision = driver.schedule(view)?;
    let encoded = decision.encode();
    view.decode_decision(&encoded).map_err(|_| Error(BAD_ABI))?;
    output[..wire::DECISION_BYTES].copy_from_slice(&encoded);
    Ok(wire::DECISION_BYTES)
}
