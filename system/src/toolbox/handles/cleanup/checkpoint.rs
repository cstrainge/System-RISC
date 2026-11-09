//! Retain a failed walk capability without holding locks across callbacks.
#![forbid(unsafe_code)]
use mac_object_table::shared::InvocationGate as _;

use crate::toolbox::handles::{InvocationGate, OwnerRef};

/// A process keeps a failed walk's unique ticket if even quarantine refused.
/// The linear attempt releases this private slot's lock before any callback.
/// It is not an owner index and supplies no object authority.
#[derive(Clone)]
pub(crate) struct Checkpoint
{
    state: alloc::sync::Arc<spinning_top::Spinlock<CheckpointState>>
}
enum CheckpointState
{
    Ready,
    Running,
    Retained(mac_object_table::walk::Stopped),
    Interrupted
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckpointError
{
    Busy,
    Retained,
    Interrupted
}
impl core::fmt::Display for CheckpointError
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        f.write_str(match self
                    {
                        Self::Busy => "a cleanup checkpoint is already running",
                        Self::Retained =>
                        {
                            "cleanup ticket remains retained after quarantine refusal"
                        }
                        Self::Interrupted =>
                        {
                            "cleanup invocation did not return; arbitrary privileged-fault \
                             containment is unproven (M-D04), a required future gate"
                        }
                    })
    }
}
impl Checkpoint
{
    pub(crate) fn try_new() -> Result<Self, alloc::alloc::AllocError>
    {
        Ok(Self { state: alloc::sync::Arc::try_new(spinning_top::Spinlock::new(CheckpointState::Ready))? })
    }

    fn access<R>(&self, action: impl FnOnce(&mut CheckpointState) -> R) -> R
    {
        InvocationGate::run(|| action(&mut self.state.lock()))
    }

    pub(crate) fn ensure_settled(&self) -> Result<(), CheckpointError>
    {
        self.access(|state| {
                match state
                {
                    CheckpointState::Ready => Ok(()),
                    CheckpointState::Running => Err(CheckpointError::Busy),
                    CheckpointState::Retained(_) => Err(CheckpointError::Retained),
                    CheckpointState::Interrupted => Err(CheckpointError::Interrupted)
                }
            })
    }

    pub(crate) fn run(&self, owner: &OwnerRef,
                      operation: impl FnOnce() -> Result<mac_object_table::walk::Report,
                                       mac_object_table::walk::Stopped>)
                      -> Result<mac_object_table::walk::Report, CheckpointError>
    {
        let retained =
            self.access(|state| {
                    match state
                    {
                        CheckpointState::Running => return Err(CheckpointError::Busy),
                        CheckpointState::Interrupted => return Err(CheckpointError::Interrupted),
                        _ => ()
                    }
                    Ok(match core::mem::replace(state, CheckpointState::Running)
                    {
                        CheckpointState::Retained(error) => Some(error),
                        _ => None
                    })
                })?;
        let mut attempt = Attempt { checkpoint: self.clone(),
                                    failure:    retained,
                                    returned:   true };
        if let Some(stopped) = attempt.failure.take()
        {
            if let Err(error) = owner.quarantine(stopped.error.into_ticket())
            {
                attempt.failure = Some(mac_object_table::walk::Stopped { report: stopped.report,
                                                                         error });
                return Err(CheckpointError::Retained);
            }
        }
        attempt.returned = false;
        let result = operation();
        attempt.returned = true;
        match result
        {
            Ok(report) => Ok(report),
            Err(error) =>
            {
                attempt.failure = Some(error);
                Err(CheckpointError::Retained)
            }
        }
    }
}
struct Attempt
{
    checkpoint: Checkpoint,
    failure:    Option<mac_object_table::walk::Stopped>,
    returned:   bool
}
impl Drop for Attempt
{
    fn drop(&mut self)
    {
        // Only this private linear attempt can change Running. Nested attempts
        // refuse before constructing a guard. No callback runs under this lock.
        let next = if let Some(error) = self.failure.take()
        {
            CheckpointState::Retained(error)
        }
        else if self.returned
        {
            CheckpointState::Ready
        }
        else
        {
            CheckpointState::Interrupted
        };
        self.checkpoint.access(|state| *state = next);
    }
}
impl core::fmt::Display for Checkpoint
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        self.access(|state| {
                match state
                {
                    CheckpointState::Ready => f.write_str("cleanup checkpoint ready"),
                    CheckpointState::Running => CheckpointError::Busy.fmt(f),
                    CheckpointState::Retained(error) => error.fmt(f),
                    CheckpointState::Interrupted => CheckpointError::Interrupted.fmt(f)
                }
            })
    }
}

