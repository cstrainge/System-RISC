//! The private policy entry uses an actual callback lease, never a broker
//! provider number or an unpinned copied Instance. Raw C dispatch lives here.
use core::fmt;

use mac_abi::{ACCESS_DENIED, BAD_ABI, OK, SERVICE_BUSY, Status};
use mac_module::abi;
use mac_scheduler_interface::{self as wire, control};

use crate::machine::traps::execution::policy;

#[derive(Debug)]
pub(crate) enum Failure
{
    Admission(Status),
    Provider(Status),
    InvalidReply,
    Scope(policy::Error),
    Clock,
    Late
    {
        elapsed: u64,
        budget:  u64
    }
}
impl Failure
{
    pub(crate) fn status(&self) -> Status
    {
        match self
        {
            Self::Admission(status) | Self::Provider(status) => *status,
            Self::InvalidReply | Self::Clock => BAD_ABI,
            Self::Scope(_) => ACCESS_DENIED,
            Self::Late { .. } => SERVICE_BUSY
        }
    }
}
impl fmt::Display for Failure
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Admission(status) =>
            {
                write!(f, "scheduler execution admission refused ({status})")
            }
            Self::Provider(status) => write!(f, "scheduler callback refused the round ({status})"),
            Self::InvalidReply =>
            {
                f.write_str("scheduler changed its call envelope or returned an invalid decision \
                             extent/status; rebuild System and the scheduler together")
            }
            Self::Scope(error) => write!(f, "scheduler attempted a forbidden operation: {error}"),
            Self::Clock =>
            {
                f.write_str("scheduler elapsed time cannot be represented by the admitted clock")
            }
            Self::Late { elapsed,
                         budget } =>
            {
                write!(f,
                       "scheduler returned after {elapsed} microseconds; admitted callback budget \
                        is {budget} microseconds")
            }
        }
    }
}

pub(crate) fn choose(lease: &super::scheduler::CallLease, packet: &[u8])
                     -> Result<wire::Decision, Failure>
{
    let round = wire::Round::decode(packet).map_err(|_| Failure::InvalidReply)?;
    let instance = lease.instance();
    let dispatch = instance.dispatch.ok_or(Failure::InvalidReply)?;
    let mut output = [0; wire::DECISION_BYTES];
    let mut call =
        abi::Call { header:          abi::Header::of::<abi::Call>(),
                    ticket:          round.context().round,
                    phase:           control::POLICY_PHASE,
                    class:           wire::POLICY_CLASS.0,
                    operation:       wire::CHOOSE,
                    input:           packet.as_ptr(),
                    input_len:       packet.len().try_into().map_err(|_| Failure::InvalidReply)?,
                    output:          output.as_mut_ptr(),
                    output_capacity: wire::DECISION_BYTES as u32,
                    output_len:      0,
                    reserved:        0 };
    let original = call;
    // Executor/resource overlays are installed before the restricted span and
    // restored after it. No registry/scheduler/packet lock crosses C dispatch.
    let (result, elapsed) = super::invocation::enter(lease.execution(), || {
                                let start = crate::platform::ticks();
                                let result = policy::invoke(|| {
                                    // SAFETY: the lease retains the validated Instance state and
                                    // code; both exclusive
                                    // output and immutable input live through return.
                                    unsafe { dispatch(instance.state, &mut call) }
                                });
                                (result, crate::platform::ticks().checked_sub(start))
                            }).map_err(Failure::Admission)?;
    let result = result.map_err(Failure::Scope)?;
    let elapsed = crate::machine::monitor::ticks_to_microseconds_ceil(elapsed.ok_or(Failure::Clock)?)
                                              .map_err(|_| Failure::Clock)?;
    let budget = lease.plan().limits().callback_budget_microseconds;
    if call.header.version != original.header.version
       || call.header.size != original.header.size
       || call.ticket != original.ticket
       || call.phase != original.phase
       || call.class != original.class
       || call.operation != original.operation
       || call.input != original.input
       || call.input_len != original.input_len
       || call.output != original.output
       || call.output_capacity != original.output_capacity
       || call.reserved != original.reserved
       || result > OK
    {
        return Err(Failure::InvalidReply);
    }
    if result != OK
    {
        return Err(Failure::Provider(result));
    }
    if call.output_len != wire::DECISION_BYTES as u32
    {
        return Err(Failure::InvalidReply);
    }
    // The core still revalidates the chosen canonical THRD and running claim.
    let decision = round.decode_decision(&output).map_err(|_| Failure::InvalidReply)?;
    // Slow malformed replies and forbidden operations remain protocol faults;
    // lateness never hides those behind an ordinary expired-sample refusal.
    if elapsed > budget
    {
        return Err(Failure::Late { elapsed,
                                   budget });
    }
    Ok(decision)
}
