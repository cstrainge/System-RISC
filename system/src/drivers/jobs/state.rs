//! Queue metadata is locked only outside foreign callbacks.
use super::*;
impl Core
{
    pub const fn new() -> Self
    {
        Self { records:              records::Records::new(),
               ready:                VecDeque::new(),
               owned_bytes:          0,
               notices:              VecDeque::new(),
               quarantined:          None,
               unreported:           0,
               #[cfg(test)]
               fixture:              None }
    }

    pub fn submit(&mut self, subject: &super::subjects::Subject, owner: Owner, work: Work)
                  -> Result<mac_object_table::projected::Publication, SubmissionError>
    {
        if let Err(cause) = subject.scope().admitting()
        {
            return Err(SubmissionError { status: crate::toolbox::handles::projection_status(
                mac_object_table::projected::Error::Pool(mac_object_table::domain::PoolError::Scope {
                    owner: subject.scope().subject(), kind: crate::toolbox::handles::KindCode(*b"DJOB"), cause })), work });
        }
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
        let node = match Box::try_new(Node { id: 0,
                                             receipt: None,
                                             owner,
                                             work: None,
                                             cancel: false,
                                             outcome: None,
                                             next: None,
                                             failure: None,
                                             reported: false,
                                             _subject: subject.clone() })
        {
            Ok(node) => node,
            Err(_) =>
            {
                return Err(SubmissionError { status: NO_MEMORY,
                                             work });
            }
        };
        let record =
            match Arc::try_new(spinning_top::Spinlock::new(Record { receipt: None,
                                                                    owner,
                                                                    waiter: if let Owner::Thread(thread) =
                                                                        owner
                                                                    {
                                                                        Some(thread)
                                                                    }
                                                                    else
                                                                    {
                                                                        None
                                                                    },
                                                                    state: State::Admitting,
                                                                    cancel: false,
                                                                    discard: false,
                                                                    footprint }))
            {
                Ok(record) => record,
                Err(_) =>
                {
                    return Err(SubmissionError { status: NO_MEMORY,
                                                 work });
                }
            };
        let publication = match self.records.insert(subject, record.clone())
        {
            Ok(publication) => publication,
            Err(status) =>
            {
                return Err(SubmissionError { status,
                                             work });
            }
        };
        let (id, receipt) = publication.into_parts();
        let mut node = node;
        node.id = id;
        node.receipt = Some(receipt);
        node.work = Some(work);
        // The queue gate still excludes dispatch/cleanup. All fallible work
        // precedes publication; no unsent input was moved on a refused insert.
        {
            let mut record = record.lock();
            record.receipt = Some(receipt);
            record.state = State::Ready(node);
        }
        self.ready.push_back(receipt);
        self.owned_bytes += footprint;
        Ok(publication)
    }

    fn owned(&self, owner: Owner, access: records::Access) -> Result<records::Guard<'_>, Status>
    {
        let record = self.records.lookup(access).ok_or(NOT_FOUND)?;
        if record.owner != owner
        {
            return Err(ACCESS_DENIED);
        }
        if matches!(record.state, State::Released)
        {
            return Err(NOT_FOUND);
        }
        Ok(record)
    }

    pub fn pending(&mut self, owner: Owner, id: u32) -> Result<bool, Status>
    {
        let record = self.owned(owner, id.into())?;
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
        let mut record = self.owned(owner, id.into())?;
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
        let record = self.records.get_mut(id).ok_or(NOT_FOUND)?;
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
        let mut record = self.records.get_mut(id).ok_or(NOT_FOUND)?;
        if record.waiter != Some(thread)
        {
            return Err(ACCESS_DENIED);
        }
        record.waiter = None;
        Ok(())
    }

    pub fn completed_waiter(&mut self, owner: Owner, access: records::Access)
                            -> Result<Option<u64>, Status>
    {
        let record = self.owned(owner, access)?;
        Ok(if matches!(record.state, State::Finished(_)) { record.waiter } else { None })
    }

    pub fn next_dispatch(&mut self) -> Option<Box<Node>>
    {
        while let Some(receipt) = self.ready.pop_front()
        {
            let Some(mut record) = self.records.lookup(records::Access::Completion(receipt))
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
    pub fn dispatch_for(&mut self, owner: Owner, access: records::Access)
                        -> Result<Option<Box<Node>>, Status>
    {
        let id = access.id();
        if !matches!(self.owned(owner, access)?.state, State::Ready(_))
        {
            return Ok(None);
        }
        let Some(index) = self.ready.iter().position(|ready| ready.id() == id)
        else
        {
            return Ok(None);
        };
        let receipt = self.ready.remove(index).ok_or(NOT_FOUND)?;
        self.ready.push_front(receipt);
        Ok(self.next_dispatch())
    }

    pub fn finish(&mut self, mut node: Box<Node>) -> Result<Option<CompletionNotice>, Box<Node>>
    {
        let Some(receipt) = node.receipt.filter(|receipt| receipt.id() == node.id)
        else
        {
            return Err(node);
        };
        let Some(mut record) =
            self.records.lookup(records::Access::Completion(receipt)).filter(|r| {
                                                                         r.owner == node.owner
                                                                         && matches!(r.state,
                                                                                     State::Running)
                                                                     })
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
            node.work = Some(work);
            record.state = State::Ready(node);
            self.ready.push_back(receipt);
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
            node.work = Some(work);
            record.state = State::Ready(node);
            self.ready.push_back(receipt);
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
                    Ok(core::mem::take(&mut work.output))
                }
                Outcome::Complete(_) => Err(Failure::Service(BAD_ABI))
            }
        };
        // Free callback buffers before publishing any cleanup ACK. A result
        // handed to the coordinator remains owned by the finished record.
        drop(work);
        if record.discard
        {
            let id = node.id;
            drop(record);
            drop(result);
            drop(node);
            self.remove(id);
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
            let Some(receipt) = node.receipt.filter(|receipt| receipt.id() == node.id)
            else
            {
                self.quarantine(node, ReturnCause::MissingOrForeign);
                return;
            };
            let Some(mut record) =
                self.records
                    .lookup(records::Access::Completion(receipt))
                    .filter(|r| r.owner == node.owner && matches!(r.state, State::Running))
            else
            {
                self.quarantine(node, ReturnCause::MissingOrForeign);
                return;
            };
            let id = node.id;
            if record.cancel && node.work.as_ref().is_some_and(|work| work.ticket == 0)
            {
                record.state = State::Finished(Completed { result:
                                                               Err(Failure::Cancelled),
                                                           cancellation_requested: true });
                let discard = record.discard;
                let notice = CompletionNotice { owner:  record.owner,
                                                waiter: record.waiter,
                                                job:    id };
                drop(record);
                drop(node);
                if discard
                {
                    self.remove(id);
                }
                else
                {
                    self.notices.push_back(notice);
                }
                return;
            }
            node.cancel = record.cancel;
            record.state = State::Ready(node);
            self.ready.push_back(receipt);
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
        if let Some(mut record) =
            node.receipt
                .filter(|receipt| receipt.id() == node.id)
                .and_then(|receipt| self.records.lookup(records::Access::Completion(receipt)))
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
        self.records.failure()
    }

    fn remove(&mut self, id: u32)
    {
        if let Some(mut record) = self.records.retained(id)
        {
            if !matches!(record.state, State::Released)
            {
                record.state = State::Released;
                self.owned_bytes -= record.footprint;
            }
        }
        self.records.remove(id);
        self.ready.retain(|queued| queued.id() != id);
        self.notices.retain(|notice| notice.job != id);
    }

    pub fn take(&mut self, owner: Owner, access: records::Access)
                -> Result<Option<Completed>, Status>
    {
        let id = access.id();
        let mut record = self.owned(owner, access)?;
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
        drop(record);
        self.remove(id);
        Ok(Some(result))
    }

    pub fn cancel(&mut self, owner: Owner, access: records::Access) -> Result<(), Status>
    {
        let id = access.id();
        let mut record = self.owned(owner, access)?;
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
            drop(record);
            self.ready.retain(|queued| queued.id() != id);
        }
        Ok(())
    }

    pub fn discard(&mut self, owner: Owner, access: records::Access) -> Result<(), Status>
    {
        let id = access.id();
        self.cancel(owner, access)?;
        let mut record = self.owned(owner, access)?;
        record.discard = true;
        if matches!(record.state, State::Finished(_))
        {
            drop(record);
            self.remove(id);
        }
        Ok(())
    }

    pub(super) fn release(&mut self, ticket: mac_object_table::domain::ReleasedResource,
                          storage: bool)
                          -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        let retiring = self.records.release(ticket, storage)?;
        if let Some((owner, receipt)) = retiring
        {
            if !matches!(owner, Owner::Kernel)
            {
                let _ = self.discard(owner, records::Access::Completion(receipt));
            }
        }
        Ok(())
    }

    pub fn discard_owner(&mut self, owner: Owner)
    {
        // A dead waiter cannot cancel a resident-owned mutation or consume its
        // result. Detach its exact thread generation before retiring app jobs.
        if let Owner::Thread(thread) = owner
        {
            for (_, mut record) in self.records.iter()
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
            if let Some(receipt) = self.records.receipt(id)
            {
                let _ = self.discard(owner, records::Access::Completion(receipt));
            }
        }
    }
}
