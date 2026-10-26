//! Queue metadata is locked only outside foreign callbacks.
use super::*;
impl Core
{
    pub const fn new() -> Self
    {
        Self { records:     Table::new(Kind::DriverJobs),
               ready:       VecDeque::new(),
               owned_bytes: 0,
               notices:     VecDeque::new(),
               quarantined: None,
               unreported:  0 }
    }

    pub fn submit(&mut self, owner: Owner, work: Work) -> Result<u32, SubmissionError>
    {
        let Some(footprint) = work.footprint().filter(|n| {
                                                  self.owned_bytes
                                                      .checked_add(*n)
                                                      .is_some_and(|total| total <= MAX_OWNED_BYTES)
                                              })
        else
        {
            return Err(SubmissionError { status: NO_MEMORY,
                                         work });
        };
        if self.ready.try_reserve(MAX_JOBS.saturating_sub(self.ready.len())).is_err()
           || self.notices.try_reserve(MAX_JOBS.saturating_sub(self.notices.len())).is_err()
        {
            return Err(SubmissionError { status: NO_MEMORY,
                                         work });
        }
        // This node also carries its own quarantine link. Cleanup never allocates.
        let node = match Box::try_new(Node { id: 0,
                                             owner,
                                             work: None,
                                             cancel: false,
                                             outcome: None,
                                             next: None,
                                             failure: None,
                                             reported: false })
        {
            Ok(node) => node,
            Err(_) =>
            {
                return Err(SubmissionError { status: NO_MEMORY,
                                             work });
            }
        };
        let mut node = Some(node);
        let mut work = Some(work);
        let result = self.records.insert_with(|id| {
                                     let mut node =
                                         node.take().expect("object table calls its publication \
                                                             factory exactly once");
                                     node.id = id;
                                     node.work = work.take();
                                     Record { owner,
                                              waiter: if let Owner::Thread(thread) = owner
                                              {
                                                  Some(thread)
                                              }
                                              else
                                              {
                                                  None
                                              },
                                              state: State::Ready(node),
                                              cancel: false,
                                              discard: false,
                                              footprint }
                                 });
        match result
        {
            Ok(id) =>
            {
                self.ready.push_back(id);
                self.owned_bytes += footprint;
                Ok(id)
            }
            Err(_) =>
            {
                Err(SubmissionError { status: NO_MEMORY,
                                      work:   work.expect("refused table publication never \
                                                           consumes the work factory") })
            }
        }
    }

    fn owned(&mut self, owner: Owner, id: u32) -> Result<&mut Record, Status>
    {
        let record = self.records.get_mut(id).ok_or(NOT_FOUND)?;
        if record.owner != owner
        {
            return Err(ACCESS_DENIED);
        }
        Ok(record)
    }

    pub fn pending(&mut self, owner: Owner, id: u32) -> Result<bool, Status>
    {
        let record = self.owned(owner, id)?;
        if matches!(record.state, State::Quarantined)
        {
            return Err(mac_abi::ROLLBACK_FAILED);
        }
        Ok(!matches!(record.state, State::Finished(_)))
    }

    /// Resident-only attachment: authority remains with owner; a waiter cannot
    /// take/cancel/discard the result or replace another thread's attachment.
    pub fn attach_waiter(&mut self, owner: Owner, id: u32, thread: u64) -> Result<(), Status>
    {
        if thread <= 1
        {
            return Err(BAD_ARGUMENT);
        }
        let record = self.owned(owner, id)?;
        match owner
        {
            Owner::Kernel => (),
            Owner::Thread(own) if own == thread => (),
            _ => return Err(ACCESS_DENIED)
        }
        if record.waiter.is_some_and(|old| old != thread)
        {
            return Err(SERVICE_BUSY);
        }
        record.waiter = Some(thread);
        Ok(())
    }

    /// The scheduler supplies the actual generation-tagged OS thread ID.
    pub fn pending_waiter(&self, thread: u64, id: u32) -> Result<bool, Status>
    {
        let record = self.records.get(id).ok_or(NOT_FOUND)?;
        if record.waiter != Some(thread)
        {
            return Err(ACCESS_DENIED);
        }
        if matches!(record.state, State::Quarantined)
        {
            return Err(mac_abi::ROLLBACK_FAILED);
        }
        Ok(!matches!(record.state, State::Finished(_)))
    }

    pub fn detach_waiter(&mut self, thread: u64, id: u32) -> Result<(), Status>
    {
        let record = self.records.get_mut(id).ok_or(NOT_FOUND)?;
        if record.waiter != Some(thread)
        {
            return Err(ACCESS_DENIED);
        }
        record.waiter = None;
        Ok(())
    }

    pub fn completed_waiter(&mut self, owner: Owner, id: u32) -> Result<Option<u64>, Status>
    {
        let record = self.owned(owner, id)?;
        Ok(if matches!(record.state, State::Finished(_)) { record.waiter } else { None })
    }

    pub fn next_dispatch(&mut self) -> Option<Box<Node>>
    {
        while let Some(id) = self.ready.pop_front()
        {
            let Some(record) = self.records.get_mut(id)
            else
            {
                continue;
            };
            if !matches!(record.state, State::Ready(_))
            {
                continue;
            }
            let state = core::mem::replace(&mut record.state, State::Running);
            if let State::Ready(mut node) = state
            {
                node.cancel = record.cancel;
                node.outcome = None;
                return Some(node);
            }
        }
        None
    }

    /// Claim one exact owned ready job without running unrelated callbacks.
    /// Dispatch ownership and completion use the ordinary queue protocol.
    pub fn dispatch_for(&mut self, owner: Owner, id: u32) -> Result<Option<Box<Node>>, Status>
    {
        if !matches!(self.owned(owner, id)?.state, State::Ready(_))
        {
            return Ok(None);
        }
        let Some(index) = self.ready.iter().position(|ready| *ready == id)
        else
        {
            return Ok(None);
        };
        self.ready.remove(index);
        self.ready.push_front(id);
        Ok(self.next_dispatch())
    }

    pub fn finish(&mut self, mut node: Box<Node>) -> Result<Option<CompletionNotice>, Box<Node>>
    {
        let Some(record) =
            self.records
                .get_mut(node.id)
                .filter(|r| r.owner == node.owner && matches!(r.state, State::Running))
        else
        {
            return Err(node);
        };
        let Some(outcome) = node.outcome.take()
        else
        {
            return Err(node);
        };
        let Some(mut work) = node.work.take()
        else
        {
            return Err(node);
        };
        if outcome == Outcome::Contended && !(record.cancel && work.ticket == 0)
        {
            let id = node.id;
            node.work = Some(work);
            record.state = State::Ready(node);
            self.ready.push_back(id);
            return Ok(None);
        }
        if work.ticket != 0
        {
            match outcome
            {
                Outcome::Pending | Outcome::Contended | Outcome::Failed(SERVICE_BUSY) => (),
                Outcome::Failed(error) => work.forced_error = Some(error),
                Outcome::Complete(_) => work.forced_error = Some(BAD_ABI)
            }
            let id = node.id;
            node.work = Some(work);
            record.state = State::Ready(node);
            self.ready.push_back(id);
            return Ok(None);
        }
        let result = if let Some(error) = work.forced_error
        {
            Err(Failure::Service(error))
        }
        else
        {
            match outcome
            {
                Outcome::Pending => Err(Failure::Service(BAD_ABI)),
                Outcome::Contended => Err(Failure::Cancelled),
                Outcome::Failed(error) => Err(Failure::Service(error)),
                Outcome::Complete(length) if length <= work.output.len() =>
                {
                    work.output.truncate(length);
                    Ok(work.output)
                }
                Outcome::Complete(_) => Err(Failure::Service(BAD_ABI))
            }
        };
        if record.discard
        {
            self.remove(node.id);
            return Ok(None);
        }
        record.state = State::Finished(Completed { result,
                                                   cancellation_requested: record.cancel });
        Ok(Some(CompletionNotice { owner:  record.owner,
                                   waiter: record.waiter,
                                   job:    node.id }))
    }

    pub(super) fn return_owned(&mut self, mut node: Box<Node>, interrupted: bool)
    {
        if interrupted
        {
            self.quarantine(node, ReturnCause::Interrupted);
            return;
        }
        if let Some(cause) = node.failure
        {
            self.quarantine(node, cause);
            return;
        }
        if node.outcome.is_none()
        {
            if let Some(record) =
                self.records
                    .get_mut(node.id)
                    .filter(|r| r.owner == node.owner && matches!(r.state, State::Running))
            {
                let id = node.id;
                if record.cancel && node.work.as_ref().is_some_and(|work| work.ticket == 0)
                {
                    record.state = State::Finished(Completed { result:
                                                                   Err(Failure::Cancelled),
                                                               cancellation_requested: true });
                    if record.discard
                    {
                        self.remove(id);
                    }
                    else
                    {
                        self.notices.push_back(CompletionNotice { owner:  record.owner,
                                                                  waiter: record.waiter,
                                                                  job:    id });
                    }
                    return;
                }
                node.cancel = record.cancel;
                record.state = State::Ready(node);
                self.ready.push_back(id);
                return;
            }
            self.quarantine(node, ReturnCause::MissingOrForeign);
            return;
        }
        match self.finish(node)
        {
            Ok(Some(notice)) => self.notices.push_back(notice),
            Ok(None) => (),
            Err(node) => self.quarantine(node, ReturnCause::RejectedCompletion)
        }
    }

    fn quarantine(&mut self, mut node: Box<Node>, cause: ReturnCause)
    {
        if let Some(record) =
            self.records
                .get_mut(node.id)
                .filter(|r| r.owner == node.owner && matches!(r.state, State::Running))
        {
            record.state = State::Quarantined;
        }
        node.failure = Some(cause);
        node.next = self.quarantined.take();
        self.quarantined = Some(node);
        self.unreported += 1;
    }

    pub fn notice(&mut self) -> Option<CompletionNotice>
    {
        self.notices.pop_front()
    }

    pub fn failure(&mut self) -> Option<ReturnFailure>
    {
        let mut at = self.quarantined.as_deref_mut();
        while let Some(node) = at
        {
            if !node.reported
            {
                if let Some(cause) = node.failure
                {
                    node.reported = true;
                    self.unreported -= 1;
                    return Some(ReturnFailure { job: node.id,
                                                owner: node.owner,
                                                cause });
                }
            }
            at = node.next.as_deref_mut();
        }
        None
    }

    fn remove(&mut self, id: u32)
    {
        if let Some(record) = self.records.remove(id)
        {
            self.owned_bytes -= record.footprint;
        }
        self.ready.retain(|queued| *queued != id);
        self.notices.retain(|notice| notice.job != id);
    }

    pub fn take(&mut self, owner: Owner, id: u32) -> Result<Option<Completed>, Status>
    {
        let record = self.owned(owner, id)?;
        if matches!(record.state, State::Quarantined)
        {
            return Err(mac_abi::ROLLBACK_FAILED);
        }
        let previous = core::mem::replace(&mut record.state, State::Running);
        let State::Finished(result) = previous
        else
        {
            record.state = previous;
            return Ok(None);
        };
        self.remove(id);
        Ok(Some(result))
    }

    pub fn cancel(&mut self, owner: Owner, id: u32) -> Result<(), Status>
    {
        let record = self.owned(owner, id)?;
        if record.cancel || matches!(record.state, State::Finished(_))
        {
            return Ok(());
        }
        record.cancel = true;
        if matches!(&record.state, State::Ready(node) if node.work.as_ref().is_some_and(|work| work.ticket == 0))
        {
            record.state = State::Finished(Completed { result:
                                                           Err(Failure::Cancelled),
                                                       cancellation_requested: true });
            self.ready.retain(|queued| *queued != id);
        }
        Ok(())
    }

    pub fn discard(&mut self, owner: Owner, id: u32) -> Result<(), Status>
    {
        self.cancel(owner, id)?;
        let record = self.owned(owner, id)?;
        record.discard = true;
        if matches!(record.state, State::Finished(_))
        {
            self.remove(id);
        }
        Ok(())
    }

    pub fn discard_owner(&mut self, owner: Owner)
    {
        // A dead waiter cannot cancel a resident-owned mutation or consume its
        // result. Detach its exact thread generation before retiring app jobs.
        if let Owner::Thread(thread) = owner
        {
            for record in self.records.values_mut()
            {
                if record.waiter == Some(thread)
                {
                    record.waiter = None;
                }
            }
        }
        let mut ids = [0; MAX_JOBS];
        let mut count = 0;
        for (id, record) in self.records.iter()
        {
            if record.owner == owner
            {
                ids[count] = id;
                count += 1;
            }
        }
        for &id in &ids[..count]
        {
            let _ = self.discard(owner, id);
        }
    }
}
