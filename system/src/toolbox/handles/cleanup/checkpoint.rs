//! Retain a failed walk capability without holding locks across callbacks.
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
pub(crate) enum CheckpointError
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

    pub(crate) fn settled(&self) -> bool
    {
        self.access(|state| matches!(state, CheckpointState::Ready))
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

#[cfg(test)]
mod tests
{
    use mac_object_table::{domain::{CapacityPolicy, CompletionError, DomainKind, KindCode,
                                    OwnerIndex},
                           registry::CleanupFailure,
                           walk::{Report, Stopped}};

    use super::*;
    fn owner() -> OwnerRef
    {
        OwnerRef::try_new(OwnerIndex::issue(DomainKind::Process).unwrap()).unwrap()
    }
    #[test]
    fn failed_walk_ticket_survives_refusal_and_returns_to_ordinary_quarantine()
    {
        let owner = owner();
        let foreign = self::owner();
        let kind = KindCode(*b"FAIL");
        owner.add_pool(kind, CapacityPolicy::GrowableUntilMemory).unwrap();
        owner.grow(kind, 1).unwrap();
        let key = owner.allocate(kind).unwrap();
        let ticket = owner.invalidate(key).unwrap();
        let checkpoint = Checkpoint::try_new().unwrap();
        let pin = checkpoint.clone();
        assert_eq!(checkpoint.run(&owner, || {
                                 Err(Stopped { report: Report::default(),
                                               error:
                                                   CompletionError::registration_mismatch(ticket) })
                             }),
                   Err(CheckpointError::Retained));
        assert!(!pin.settled());
        assert!(alloc::format!("{pin}").contains("death barrier remains closed"));
        assert_eq!(owner.summary().pending, 1);
        assert_eq!(pin.run(&foreign, || panic!("foreign restoration ran cleanup")),
                   Err(CheckpointError::Retained));
        assert!(alloc::format!("{pin}").contains("owner"));
        assert_eq!(owner.summary().pending, 1);
        pin.run(&owner, || {
               assert_eq!(owner.summary().quarantined, 1);
               let ticket = owner.resume_quarantined().unwrap();
               owner.complete_release(ticket).unwrap();
               Ok(Report::default())
           })
           .unwrap();
        assert!(checkpoint.settled());
        assert_eq!((owner.summary().live, owner.summary().pending), (0, 0));
    }
    #[test]
    fn nested_checkpoint_refuses_without_locking_callbacks_or_losing_outer_completion()
    {
        let owner = owner();
        let checkpoint = Checkpoint::try_new().unwrap();
        checkpoint.run(&owner, || {
                      assert!(!checkpoint.settled());
                      assert_eq!(checkpoint.run(&owner, || panic!("nested cleanup ran")),
                                 Err(CheckpointError::Busy));
                      Ok(Report::default())
                  })
                  .unwrap();
        assert!(checkpoint.settled());
    }
    #[cfg(feature = "test-support")]
    #[test]
    fn host_transport_unwind_does_not_falsely_qualify_privileged_recovery()
    {
        let owner = owner();
        let checkpoint = Checkpoint::try_new().unwrap();
        let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                                   let _ = checkpoint.run(&owner, || {
                                                                         panic!("host transport \
                                                                                 interrupted")
                                                                     });
                                               }));
        assert!(failure.is_err());
        assert!(!checkpoint.settled());
        assert_eq!(checkpoint.run(&owner, || panic!("unproven recovery attempted")),
                   Err(CheckpointError::Interrupted));
        assert!(alloc::format!("{checkpoint}").contains("unproven"));
    }
}
