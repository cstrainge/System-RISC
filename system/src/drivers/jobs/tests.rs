use alloc::vec;

use super::*;
const CLASS: ClassId = ClassId(*b"TEST");
const OWNER: Owner = Owner::Thread(42);
fn work(bytes: usize) -> Work
{
    Work::new(1, CLASS, 7, vec![1, 2, 3], bytes).unwrap()
}
struct Script<F>(F);
impl<F: FnMut(Call<'_>) -> Outcome> Broker for Script<F>
{
    fn invoke(&mut self, call: Call<'_>) -> Outcome
    {
        (self.0)(call)
    }
}
fn step(queue: &Queue, action: impl FnMut(Call<'_>) -> Outcome) -> Option<CompletionNotice>
{
    let mut dispatch = queue.next_dispatch().unwrap();
    dispatch.invoke(&mut Script(action)).unwrap();
    queue.finish(dispatch).unwrap()
}
#[test]
fn completion_owns_result_and_retires_generation_with_separate_owner_checks()
{
    let q = Queue::new();
    let first = q.submit(OWNER, work(8)).unwrap();
    let first_key = q.test_access(|core| core.records.key(first).unwrap());
    assert_eq!(q.take(Owner::Thread(43), first).unwrap_err(), ACCESS_DENIED);
    assert!(q.take(OWNER, first).unwrap().is_none());
    assert_eq!(step(&q, |call| {
                   assert_eq!(call.owner, OWNER);
                   assert_eq!(call.input, [1, 2, 3]);
                   assert_eq!(call.provider, 1);
                   assert_eq!(call.class, CLASS);
                   assert_eq!(call.operation, 7);
                   assert_eq!(call.phase, Phase::Start);
                   assert_eq!(*call.ticket, 0);
                   call.output[..2].copy_from_slice(b"ok");
                   Outcome::Complete(2)
               }),
               Some(CompletionNotice { owner:  OWNER,
                                       waiter: Some(42),
                                       job:    first }));
    assert_eq!(q.take(OWNER, first).unwrap().unwrap().result.unwrap(), b"ok");
    let next = q.submit(OWNER, work(8)).unwrap();
    let next_key = q.test_access(|core| core.records.key(next).unwrap());
    assert!(first > u64::from(u32::MAX) && next > u64::from(u32::MAX));
    assert_eq!(first_key.slot(), next_key.slot());
    assert_ne!(first_key.generation(), next_key.generation());
    assert_ne!(first, next);
    assert_eq!(q.cancel(OWNER, first).unwrap_err(), NOT_FOUND);
    let other = Queue::new();
    let foreign = other.submit(OWNER, work(8)).unwrap();
    assert_ne!(foreign, next);
    assert_eq!(q.take(OWNER, foreign).unwrap_err(), NOT_FOUND);
    q.discard(OWNER, next).unwrap();
    other.discard(OWNER, foreign).unwrap();
}

#[test]
fn resident_result_survives_waiter_exit_and_delivers_an_acknowledged_mutation()
{
    let q = Queue::new();
    let id = q.submit(Owner::Kernel, work(8)).unwrap();
    q.attach_waiter(Owner::Kernel, id, 42).unwrap();
    assert!(q.pending_waiter(42, id).unwrap());
    assert_eq!(q.pending_waiter(43, id), Err(ACCESS_DENIED));
    assert_eq!(q.attach_waiter(Owner::Thread(42), id, 42), Err(ACCESS_DENIED));
    assert_eq!(q.attach_waiter(Owner::Kernel, id, 43), Err(SERVICE_BUSY));
    step(&q, |call| {
        *call.ticket = 91;
        Outcome::Pending
    });
    q.discard_owner(Owner::Thread(42));
    assert_eq!(q.pending_waiter(42, id), Err(ACCESS_DENIED));
    assert!(q.pending(Owner::Kernel, id).unwrap());
    let notice = step(&q, |call| {
                     assert_eq!(call.owner, Owner::Kernel);
                     assert_eq!(call.phase, Phase::Poll); // Waiter exit did not cancel I/O.
                     *call.ticket = 0;
                     call.output[..4].copy_from_slice(b"done");
                     Outcome::Complete(4)
                 }).unwrap();
    assert_eq!(notice,
               CompletionNotice { owner:  Owner::Kernel,
                                  waiter: None,
                                  job:    id });
    assert_eq!(q.take(Owner::Thread(42), id).unwrap_err(), ACCESS_DENIED);
    assert_eq!(q.take(Owner::Kernel, id).unwrap().unwrap().result.unwrap(), b"done");
}

#[test]
fn completion_before_wait_attachment_and_before_park_never_loses_readiness()
{
    let q = Queue::new();
    let id = q.submit(Owner::Kernel, work(8)).unwrap();
    let old_key = q.test_access(|core| core.records.key(id).unwrap());
    step(&q, |_| Outcome::Complete(0));
    q.attach_waiter(Owner::Kernel, id, 42).unwrap();
    assert!(!q.pending_waiter(42, id).unwrap());
    assert_eq!(q.completed_waiter(Owner::Kernel, id).unwrap(), Some(42));
    q.take(Owner::Kernel, id).unwrap().unwrap();
    assert_eq!(q.pending_waiter(42, id), Err(NOT_FOUND));
    let next = q.submit(Owner::Kernel, work(8)).unwrap();
    let next_key = q.test_access(|core| core.records.key(next).unwrap());
    assert_eq!(old_key.slot(), next_key.slot());
    assert_ne!(old_key.generation(), next_key.generation());
    assert_ne!(id, next);
    q.attach_waiter(Owner::Kernel, next, 43).unwrap();
    assert_eq!(q.pending_waiter(42, next), Err(ACCESS_DENIED));
    assert_eq!(q.detach_waiter(42, next), Err(ACCESS_DENIED));
    q.cancel(Owner::Kernel, next).unwrap();
    assert_eq!(q.completed_waiter(Owner::Kernel, next).unwrap(), Some(43));
    assert!(!q.pending_waiter(43, next).unwrap());
    q.take(Owner::Kernel, next).unwrap().unwrap();
}

#[test]
fn waiter_detach_and_explicit_cancel_preserve_resident_ownership_and_actual_ack()
{
    let q = Queue::new();
    let id = q.submit(Owner::Kernel, work(8)).unwrap();
    assert_eq!(q.attach_waiter(Owner::Kernel, id, 1), Err(BAD_ARGUMENT));
    q.attach_waiter(Owner::Kernel, id, 42).unwrap();
    q.detach_waiter(42, id).unwrap();
    q.attach_waiter(Owner::Kernel, id, 43).unwrap();
    step(&q, |call| {
        *call.ticket = 92;
        Outcome::Pending
    });
    q.cancel(Owner::Kernel, id).unwrap();
    let notice = step(&q, |call| {
                     assert_eq!(call.phase, Phase::Cancel);
                     *call.ticket = 0;
                     call.output[..4].copy_from_slice(b"done");
                     Outcome::Complete(4)
                 }).unwrap();
    assert_eq!(notice.waiter, Some(43));
    let result = q.take(Owner::Kernel, id).unwrap().unwrap();
    assert!(result.cancellation_requested);
    assert_eq!(result.result.unwrap(), b"done");
    let module = q.submit(Owner::Module(7), work(8)).unwrap();
    assert_eq!(q.attach_waiter(Owner::Module(7), module, 43), Err(ACCESS_DENIED));
    q.discard(Owner::Module(7), module).unwrap();
}
#[test]
fn cancel_before_dispatch_never_contacts_provider_and_repeated_cancel_is_no_change()
{
    let q = Queue::new();
    let id = q.submit(OWNER, work(8)).unwrap();
    q.cancel(OWNER, id).unwrap();
    q.cancel(OWNER, id).unwrap();
    assert!(!q.has_ready());
    assert!(q.next_dispatch().is_none());
    let result = q.take(OWNER, id).unwrap().unwrap();
    assert_eq!(result.result.unwrap_err(), Failure::Cancelled);
    assert!(result.cancellation_requested);
    assert_eq!(q.owned_bytes(), 0);
}
#[test]
fn pending_cancellation_drains_actual_result_without_claiming_mutation_was_undone()
{
    let q = Queue::new();
    let id = q.submit(OWNER, work(8)).unwrap();
    assert_eq!(step(&q, |call| {
                   *call.ticket = 91;
                   Outcome::Pending
               }),
               None);
    q.cancel(OWNER, id).unwrap();
    assert_eq!(step(&q, |call| {
                   assert_eq!(call.phase, Phase::Cancel);
                   assert_eq!(*call.ticket, 91);
                   assert!(call.input.is_empty());
                   Outcome::Pending
               }),
               None);
    assert!(q.take(OWNER, id).unwrap().is_none());
    step(&q, |call| {
        assert_eq!(call.phase, Phase::Cancel);
        *call.ticket = 0;
        call.output[..4].copy_from_slice(b"done");
        Outcome::Complete(4)
    });
    let result = q.take(OWNER, id).unwrap().unwrap();
    assert_eq!(result.result.unwrap(), b"done");
    assert!(result.cancellation_requested);
}
#[test]
fn thread_exit_retains_buffers_and_ticket_until_provider_acknowledges_cancellation()
{
    let q = Queue::new();
    let id = q.submit(OWNER, work(8)).unwrap();
    step(&q, |call| {
        *call.ticket = 99;
        Outcome::Pending
    });
    let footprint = q.owned_bytes();
    q.discard_owner(OWNER);
    assert_eq!(q.owned_bytes(), footprint);
    assert!(q.has_ready());
    step(&q, |call| {
        assert_eq!(call.phase, Phase::Cancel);
        Outcome::Pending
    });
    assert_eq!(q.owned_bytes(), footprint);
    step(&q, |call| {
        assert_eq!(*call.ticket, 99);
        *call.ticket = 0;
        Outcome::Complete(0)
    });
    assert_eq!(q.owned_bytes(), 0);
    assert!(!q.has_ready());
    assert_eq!(q.take(OWNER, id).unwrap_err(), NOT_FOUND);
}
#[test]
fn cancellation_during_callback_is_applied_to_next_poll_and_completed_result()
{
    let q = Queue::new();
    let id = q.submit(OWNER, work(8)).unwrap();
    let mut dispatch = q.next_dispatch().unwrap();
    q.cancel(OWNER, id).unwrap();
    dispatch.invoke(&mut Script(|call: Call<'_>| {
                        *call.ticket = 100;
                        Outcome::Pending
                    }))
            .unwrap();
    q.finish(dispatch).unwrap();
    step(&q, |call| {
        assert_eq!(call.phase, Phase::Cancel);
        *call.ticket = 0;
        Outcome::Complete(0)
    });
    assert!(q.take(OWNER, id).unwrap().unwrap().cancellation_requested);
}
#[test]
fn pending_jobs_are_fair_and_keep_borrowed_input_out_of_polls()
{
    let q = Queue::new();
    let first = q.submit(OWNER, work(8)).unwrap();
    let second = q.submit(OWNER, work(8)).unwrap();
    step(&q, |call| {
        assert_eq!(call.phase, Phase::Start);
        *call.ticket = 12;
        Outcome::Pending
    });
    assert_eq!(step(&q, |call| {
                   assert_eq!(call.phase, Phase::Start);
                   Outcome::Complete(0)
               }),
               Some(CompletionNotice { owner:  OWNER,
                                       waiter: Some(42),
                                       job:    second }));
    assert_eq!(step(&q, |call| {
                   assert_eq!(call.phase, Phase::Poll);
                   assert_eq!(*call.ticket, 12);
                   assert!(call.input.is_empty());
                   *call.ticket = 0;
                   Outcome::Complete(0)
               }),
               Some(CompletionNotice { owner:  OWNER,
                                       waiter: Some(42),
                                       job:    first }));
    q.take(OWNER, first).unwrap();
    q.take(OWNER, second).unwrap();
}
#[test]
fn pre_dispatch_contention_keeps_ticket_and_does_not_restart_request()
{
    let q = Queue::new();
    let id = q.submit(OWNER, work(8)).unwrap();
    step(&q, |call| {
        *call.ticket = 13;
        Outcome::Pending
    });
    step(&q, |call| {
        assert_eq!(call.phase, Phase::Poll);
        Outcome::Failed(SERVICE_BUSY)
    });
    step(&q, |call| {
        assert_eq!(call.phase, Phase::Poll);
        assert_eq!(*call.ticket, 13);
        *call.ticket = 0;
        Outcome::Failed(NOT_FOUND)
    });
    assert_eq!(q.take(OWNER, id).unwrap().unwrap().result.unwrap_err(),
               Failure::Service(NOT_FOUND));
}
#[test]
fn malformed_completion_with_live_ticket_is_cancelled_before_reporting_error()
{
    let q = Queue::new();
    let id = q.submit(OWNER, work(8)).unwrap();
    step(&q, |call| {
        *call.ticket = 14;
        Outcome::Complete(0)
    });
    assert!(q.take(OWNER, id).unwrap().is_none());
    step(&q, |call| {
        assert_eq!(call.phase, Phase::Cancel);
        *call.ticket = 0;
        Outcome::Complete(0)
    });
    assert_eq!(q.take(OWNER, id).unwrap().unwrap().result.unwrap_err(), Failure::Service(BAD_ABI));
}
#[test]
fn foreign_or_uninvoked_dispatch_is_returned_intact_not_dropped()
{
    let q = Queue::new();
    let id = q.submit(OWNER, work(8)).unwrap();
    let dispatch = q.next_dispatch().unwrap();
    let mut dispatch = q.finish(dispatch).unwrap_err();
    dispatch.invoke(&mut Script(|call: Call<'_>| {
                        *call.ticket = 15;
                        Outcome::Pending
                    }))
            .unwrap();
    let other = Queue::new();
    let dispatch = other.finish(dispatch).unwrap_err();
    assert_eq!(dispatch.ticket().unwrap(), 15);
    q.finish(dispatch).unwrap();
    step(&q, |call| {
        assert_eq!(*call.ticket, 15);
        *call.ticket = 0;
        Outcome::Complete(0)
    });
    q.take(OWNER, id).unwrap();
}
#[test]
fn memory_budget_fails_before_dispatch_and_returns_unsent_owned_input()
{
    let q = Queue::new();
    let id = q.submit(OWNER, Work::new(1, CLASS, 7, Vec::new(), MAX_OWNED_BYTES).unwrap()).unwrap();
    let error = q.submit(OWNER, work(8)).unwrap_err();
    assert_eq!(error.status, NO_MEMORY);
    assert_eq!(error.work.input, [1, 2, 3]);
    q.discard(OWNER, id).unwrap();
    assert_eq!(q.owned_bytes(), 0);
    let id = q.submit(OWNER, error.work).unwrap();
    q.discard(OWNER, id).unwrap();
}

#[test]
fn admission_contention_before_start_retains_owned_work_and_can_be_cancelled_unsent()
{
    let q = Queue::new();
    let id = q.submit(OWNER, work(8)).unwrap();
    step(&q, |call| {
        assert_eq!(*call.ticket, 0);
        Outcome::Contended
    });
    assert!(q.pending(OWNER, id).unwrap());
    step(&q, |call| {
        assert_eq!(call.phase, Phase::Start);
        assert_eq!(call.input, [1, 2, 3]);
        Outcome::Contended
    });
    q.cancel(OWNER, id).unwrap();
    assert!(!q.pending(OWNER, id).unwrap());
    assert_eq!(q.take(OWNER, id).unwrap().unwrap().result.unwrap_err(), Failure::Cancelled);
}

#[test]
fn cancellation_during_contended_admission_never_retries_the_unsent_start()
{
    let q = Queue::new();
    let id = q.submit(OWNER, work(8)).unwrap();
    let mut dispatch = q.next_dispatch().unwrap();
    q.cancel(OWNER, id).unwrap();
    dispatch.invoke(&mut Script(|_: Call<'_>| Outcome::Contended)).unwrap();
    q.finish(dispatch).unwrap();
    assert!(!q.has_ready());
    assert_eq!(q.take(OWNER, id).unwrap().unwrap().result.unwrap_err(), Failure::Cancelled);
}

#[test]
fn exact_dispatch_preserves_other_work_and_rejects_foreign_cancelled_or_stale_jobs()
{
    let q = Queue::new();
    let earlier = q.submit(OWNER, work(8)).unwrap();
    let immediate = q.submit(Owner::Kernel, Work::new(2, CLASS, 9, vec![], 0).unwrap()).unwrap();
    assert_eq!(q.dispatch_for(OWNER, immediate).unwrap_err(), ACCESS_DENIED);
    let mut selected = q.dispatch_for(Owner::Kernel, immediate).unwrap().unwrap();
    selected.invoke(&mut Script(|call: Call<'_>| {
                        assert_eq!(call.provider, 2);
                        assert_eq!(call.operation, 9);
                        Outcome::Complete(0)
                    }))
            .unwrap();
    q.finish(selected).unwrap();
    assert!(q.dispatch_for(Owner::Kernel, immediate).unwrap().is_none());
    assert!(q.take(OWNER, earlier).unwrap().is_none(), "exact dispatch ran an unrelated callback");
    q.take(Owner::Kernel, immediate).unwrap().unwrap();
    assert_eq!(q.dispatch_for(Owner::Kernel, immediate).unwrap_err(), NOT_FOUND);
    let cancelled = q.submit(Owner::Kernel, work(0)).unwrap();
    q.cancel(Owner::Kernel, cancelled).unwrap();
    assert!(q.dispatch_for(Owner::Kernel, cancelled).unwrap().is_none());
    assert_eq!(step(&q, |call| {
                   assert_eq!(call.owner, OWNER);
                   Outcome::Complete(0)
               }).unwrap()
                 .job,
               earlier);
    q.take(OWNER, earlier).unwrap().unwrap();
    q.take(Owner::Kernel, cancelled).unwrap().unwrap();
    assert!(q.next_dispatch().is_none());
}

#[test]
fn dropped_uninvoked_guard_restores_work_without_contacting_the_provider()
{
    let q = Queue::new();
    let id = q.submit(OWNER, work(8)).unwrap();
    let before = q.owned_bytes();
    drop(q.next_dispatch().unwrap());
    assert!(q.has_ready());
    assert_eq!(q.owned_bytes(), before);
    step(&q, |call| {
        assert_eq!(call.phase, Phase::Start);
        assert_eq!(call.input, [1, 2, 3]);
        Outcome::Complete(0)
    });
    assert!(q.take(OWNER, id).unwrap().unwrap().result.is_ok());
}

#[test]
fn uninvoked_guard_drop_after_cancel_never_starts_an_unsent_request()
{
    let q = Queue::new();
    let id = q.submit(OWNER, work(8)).unwrap();
    let dispatch = q.next_dispatch().unwrap();
    q.cancel(OWNER, id).unwrap();
    drop(dispatch);
    assert!(q.next_dispatch().is_none());
    assert_eq!(q.take(OWNER, id).unwrap().unwrap().result, Err(Failure::Cancelled));
    assert!(q.take_notice().is_none(), "taking the result must retire a queued wake notice");
    assert!(!q.has_ready());
}

#[test]
fn dropped_pending_guard_preserves_the_exact_ticket_and_cancellation_ack()
{
    let q = Queue::new();
    let id = q.submit(OWNER, work(8)).unwrap();
    let mut dispatch = q.next_dispatch().unwrap();
    dispatch.invoke(&mut Script(|call: Call<'_>| {
                        *call.ticket = 77;
                        Outcome::Pending
                    }))
            .unwrap();
    drop(dispatch);
    q.discard_owner(OWNER);
    assert!(q.owned_bytes() > 0);
    step(&q, |call| {
        assert_eq!(call.phase, Phase::Cancel);
        assert_eq!(*call.ticket, 77);
        *call.ticket = 0;
        Outcome::Complete(0)
    });
    assert_eq!(q.owned_bytes(), 0);
    assert_eq!(q.take(OWNER, id).unwrap_err(), NOT_FOUND);
    assert!(!q.has_ready());
}

#[test]
fn dropping_a_foreign_finish_refusal_returns_to_its_actual_origin_and_wakes_once()
{
    let q = Queue::new();
    let other = Queue::new();
    let id = q.submit(OWNER, work(8)).unwrap();
    let mut dispatch = q.next_dispatch().unwrap();
    dispatch.invoke(&mut Script(|call: Call<'_>| {
                        call.output[0] = 42;
                        Outcome::Complete(1)
                    }))
            .unwrap();
    drop(other.finish(dispatch).unwrap_err());
    assert!(!other.has_ready());
    assert_eq!(q.take_notice(),
               Some(CompletionNotice { owner:  OWNER,
                                       waiter: Some(42),
                                       job:    id }));
    assert!(q.take_notice().is_none());
    assert_eq!(q.take(OWNER, id).unwrap().unwrap().result.unwrap(), [42]);
    assert!(!q.has_ready());
}

#[test]
fn guard_rejects_double_invocation_without_running_the_callback_again()
{
    let q = Queue::new();
    let id = q.submit(OWNER, work(8)).unwrap();
    let mut dispatch = q.next_dispatch().unwrap();
    dispatch.invoke(&mut Script(|_: Call<'_>| Outcome::Complete(0))).unwrap();
    assert_eq!(dispatch.invoke(&mut Script(|_: Call<'_>| panic!("duplicate callback ran"))),
               Err(BAD_ABI));
    drop(dispatch);
    q.take(OWNER, id).unwrap().unwrap().result.unwrap();
    assert!(!q.has_ready());
}

#[cfg(feature = "test-support")]
#[test]
fn host_callback_unwind_is_quarantined_and_does_not_claim_privileged_recovery()
{
    let q = Queue::new();
    let id = q.submit(OWNER, work(8)).unwrap();
    let bytes = q.owned_bytes();
    let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                                   let mut dispatch = q.next_dispatch().unwrap();
                                                   let _ = dispatch.invoke(&mut Script(|call: Call<'_>| { *call.ticket = 88; panic!("host-only callback interruption") }));
                                               }));
    assert!(interrupted.is_err());
    assert_eq!(q.owned_bytes(), bytes);
    assert_eq!(q.pending(OWNER, id), Err(mac_abi::ROLLBACK_FAILED));
    let failure = q.take_return_failure().unwrap();
    assert_eq!(failure,
               ReturnFailure { job:   id,
                               owner: OWNER,
                               cause: ReturnCause::Interrupted });
    let message = alloc::format!("{}", failure.cause);
    assert!(message.contains("unproven (M-D04)") && message.contains("restart System"),
            "{message}");
    q.discard_owner(OWNER);
    assert_eq!(q.owned_bytes(), bytes);
    assert_eq!(q.take(OWNER, id).unwrap_err(), mac_abi::ROLLBACK_FAILED);
    assert!(q.next_dispatch().is_none());
    assert!(q.take_return_failure().is_none(),
            "unchanged quarantine must not generate repeated reports");
    assert!(!q.has_ready());
}

#[cfg(feature = "test-support")]
#[test]
fn another_host_thread_can_cancel_while_a_provider_callback_owns_the_dispatch()
{
    use std::{sync::mpsc, thread, time::Duration};
    let q = Queue::new();
    let id = q.submit(OWNER, work(8)).unwrap();
    let (entered, started) = mpsc::channel();
    let (release, released) = mpsc::channel();
    thread::scope(|scope| {
        let mut dispatch = q.next_dispatch().unwrap();
        let worker = scope.spawn(move || {
                              dispatch.invoke(&mut Script(|call: Call<'_>| {
                                                  *call.ticket = 91;
                                                  entered.send(()).unwrap();
                                                  released.recv_timeout(Duration::from_secs(5))
                                                          .unwrap();
                                                  Outcome::Pending
                                              }))
                                      .unwrap();
                              drop(dispatch);
                          });
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        q.cancel(OWNER, id).unwrap();
        release.send(()).unwrap();
        worker.join().unwrap();
    });
    step(&q, |call| {
        assert_eq!(call.phase, Phase::Cancel);
        assert_eq!(*call.ticket, 91);
        *call.ticket = 0;
        Outcome::Complete(0)
    });
    assert!(q.take(OWNER, id).unwrap().unwrap().cancellation_requested);
    assert_eq!(q.owned_bytes(), 0);
}

// Only the host transport differs: all payloads, scopes, receipts and cleanup
// guards below use the same registered stores as the resident queue.
pub(super) struct Fixture
{
    _registry:  crate::toolbox::handles::Registry<()>,
    operations: alloc::sync::Arc<super::subjects::Operations>,
    owners:     Vec<(Owner, crate::toolbox::handles::OwnerRef)>
}
struct RetainTicket;
impl mac_object_table::registry::Release<(), crate::toolbox::handles::cleanup::Failure>
    for RetainTicket
{
    fn release(&self, _: &mut (), ticket: mac_object_table::domain::ReleasedResource)
               -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        Err(crate::toolbox::handles::cleanup::Failure::context_unavailable(ticket,
                                                                           "queue test context"))
    }
}
pub(super) fn subject(core: &mut Core, owner: Owner) -> Result<super::subjects::Subject, Status>
{
    use mac_object_table::domain::{DomainKind, OwnerIndex};

    use crate::toolbox::handles::{OwnerRef, Registry};
    let issue = || {
        OwnerRef::try_new(OwnerIndex::issue(DomainKind::Process).map_err(|_|NO_MEMORY)?)
        .map_err(|_|NO_MEMORY)
    };
    if core.fixture.is_none()
    {
        let registrar = issue()?;
        let mut registry = Registry::new();
        let mut kind = |specification| {
            registry.register(&registrar, specification, RetainTicket)
                    .map_err(|error| crate::toolbox::handles::registry_status(&error))
        };
        let roots = (kind(super::subjects::specification(false))?,
                     kind(super::subjects::specification(true))?);
        let jobs = (kind(super::records::specification(false))?,
                    kind(super::records::specification(true))?);
        core.records.prepare(jobs)?;
        core.fixture = Some(Fixture { _registry:  registry,
                                      operations: super::subjects::Operations::new(roots)?,
                                      owners:     Vec::new() });
    }
    let fixture = core.fixture.as_mut().ok_or(BAD_ABI)?;
    let authority = if let Some((_, authority)) =
        fixture.owners.iter().find(|(policy, _)| *policy == owner)
    {
        authority.clone()
    }
    else
    {
        fixture.owners.try_reserve(1).map_err(|_| NO_MEMORY)?;
        let authority = issue()?;
        fixture.owners.push((owner, authority.clone()));
        authority
    };
    fixture.operations.admit(&crate::toolbox::handles::subject::Authority::Client(authority))
}

pub(super) fn admitted(
    queue: &Queue, policy: Owner)
    -> (crate::toolbox::handles::OwnerRef, subjects::Subject, mac_object_table::store::Publication)
{
    queue.test_access(|core| {
             let subject = subject(core, policy).unwrap();
             let owner = core.fixture
                             .as_ref()
                             .unwrap()
                             .owners
                             .iter()
                             .find(|(kind, _)| *kind == policy)
                             .unwrap()
                             .1
                             .clone();
             let publication = core.submit(&subject, policy, work(8)).unwrap();
             (owner, subject, publication)
         })
}
pub(super) fn invalidate_jobs(queue: &Queue, owner: &crate::toolbox::handles::OwnerRef)
                              -> Vec<mac_object_table::domain::ReleasedResource>
{
    use mac_object_table::registry::CleanupFailure;
    let operations = queue.test_access(|core| core.fixture.as_ref().unwrap().operations.clone());
    owner.stop_admission();
    let mut deferred = Vec::new();
    while let Some(ticket) = owner.invalidate_next()
    {
        let result = match &ticket.key().kind().0
        {
            b"DIOP" => operations.release(ticket, false),
            b"DIOB" => operations.release(ticket, true),
            b"DJOB" => queue.release(ticket, false),
            b"DJST" => queue.release(ticket, true),
            other => panic!("unexpected registered kind {other:?}")
        };
        if let Err(error) = result
        {
            assert!(!alloc::format!("{error}").is_empty());
            deferred.push(error.into_parts().0);
        }
    }
    deferred
}
pub(super) fn settle(queue: &Queue, owner: &crate::toolbox::handles::OwnerRef,
                     deferred: Vec<mac_object_table::domain::ReleasedResource>)
{
    let operations = queue.test_access(|core| core.fixture.as_ref().unwrap().operations.clone());
    for ticket in deferred
    {
        match &ticket.key().kind().0
        {
            b"DIOB" => operations.release(ticket, true).unwrap(),
            b"DJST" => queue.release(ticket, true).unwrap(),
            other => panic!("unexpected retained kind {other:?}")
        }
    }
    assert_eq!((owner.summary().live, owner.summary().pending), (0, 0));
    assert_eq!(queue.owned_bytes(), 0);
}
#[test]
fn invalidated_job_rejects_ordinary_ids_but_receipt_drains_the_exact_resident_result()
{
    let queue = Queue::new();
    let (owner, subject, publication) = admitted(&queue, Owner::Kernel);
    let (id, receipt) = publication.into_parts();
    step(&queue, |call| {
        *call.ticket = 71;
        Outcome::Pending
    });
    let root = subject.scope().root().unwrap();
    let deferred = invalidate_jobs(&queue, &owner);
    assert!(owner.pending(root));
    assert_eq!(queue.pending(Owner::Kernel, id), Err(NOT_FOUND));
    assert_eq!(queue.take(Owner::Kernel, id).unwrap_err(), NOT_FOUND);
    assert_eq!(queue.cancel_completion(Owner::Thread(42), receipt), Err(ACCESS_DENIED));
    step(&queue, |call| {
        assert_eq!(call.phase, Phase::Poll, "the dead waiter must not discard a resident mutation");
        assert_eq!(*call.ticket, 71);
        *call.ticket = 0;
        call.output[..4].copy_from_slice(b"done");
        Outcome::Complete(4)
    });
    let done = queue.take_completion(Owner::Kernel, receipt).unwrap().unwrap();
    assert_eq!(done.result.unwrap(), b"done");
    assert_eq!(queue.take_completion(Owner::Kernel, receipt).unwrap_err(), NOT_FOUND);
    drop(subject);
    settle(&queue, &owner, deferred);
}
#[test]
fn owner_walk_cancels_thread_job_and_holds_payload_until_real_lower_ack()
{
    let queue = Queue::new();
    let (owner, subject, publication) = admitted(&queue, OWNER);
    let id = publication.id();
    step(&queue, |call| {
        *call.ticket = 72;
        Outcome::Pending
    });
    let deferred = invalidate_jobs(&queue, &owner);
    drop(subject);
    assert!(owner.summary().pending > 0);
    assert!(queue.owned_bytes() > 0);
    step(&queue, |call| {
        assert_eq!(call.phase, Phase::Cancel);
        assert_eq!(*call.ticket, 72);
        Outcome::Pending
    });
    assert!(queue.owned_bytes() > 0, "a cancellation request is not an acknowledgement");
    step(&queue, |call| {
        assert_eq!(call.phase, Phase::Cancel);
        *call.ticket = 0;
        Outcome::Complete(0)
    });
    assert_eq!(queue.pending(OWNER, id), Err(NOT_FOUND));
    settle(&queue, &owner, deferred);
}
#[test]
fn completion_before_cleanup_dispatch_retains_unique_ticket_and_native_identity()
{
    let queue = Queue::new();
    let (owner, subject, publication) = admitted(&queue, Owner::Kernel);
    let (id, receipt) = publication.into_parts();
    step(&queue, |call| {
        *call.ticket = 73;
        Outcome::Pending
    });
    owner.stop_admission();
    let mut tickets = Vec::new();
    while let Some(ticket) = owner.invalidate_next()
    {
        tickets.push(ticket);
    }
    step(&queue, |call| {
        assert_eq!(*call.ticket, 73);
        *call.ticket = 0;
        Outcome::Complete(0)
    });
    queue.take_completion(Owner::Kernel, receipt).unwrap().unwrap();
    assert!(owner.summary().pending > 0, "a delayed unique dispatcher still owns the ticket");
    assert_eq!(queue.take(Owner::Kernel, id).unwrap_err(), NOT_FOUND);
    let operations = queue.test_access(|core| core.fixture.as_ref().unwrap().operations.clone());
    let mut deferred = Vec::new();
    use mac_object_table::registry::CleanupFailure;
    for ticket in tickets
    {
        let result = match &ticket.key().kind().0
        {
            b"DIOP" => operations.release(ticket, false),
            b"DIOB" => operations.release(ticket, true),
            b"DJOB" => queue.release(ticket, false),
            b"DJST" => queue.release(ticket, true),
            other => panic!("unexpected kind {other:?}")
        };
        if let Err(error) = result
        {
            deferred.push(error.into_parts().0);
        }
    }
    drop(subject);
    settle(&queue, &owner, deferred);
    let (_, next_subject, next) = admitted(&queue, Owner::Thread(77));
    assert_ne!(next.id(), id);
    assert_eq!(queue.take_completion(Owner::Thread(77), receipt).unwrap_err(), NOT_FOUND);
    queue.discard(Owner::Thread(77), next.id()).unwrap();
    drop(next_subject);
}

#[test]
fn stopped_owner_can_admit_cleanup_while_earlier_invalidated_jobs_still_own_the_backing()
{
    let queue = Queue::new();
    let (owner, subject, old) = admitted(&queue, Owner::Kernel);
    let (_, old_receipt) = old.into_parts();
    step(&queue, |call| {
        *call.ticket = 91;
        Outcome::Pending
    });
    let deferred = invalidate_jobs(&queue, &owner);
    assert_eq!(queue.pending(Owner::Kernel, old.id()), Err(NOT_FOUND));
    let cleanup = queue.test_access(|core| core.submit(&subject, Owner::Kernel, work(0))).unwrap();
    let (_, cleanup_receipt) = cleanup.into_parts();
    let more_deferred = invalidate_jobs(&queue, &owner);
    assert!(more_deferred.is_empty());
    let mut cleanup_call =
        queue.dispatch_completion(Owner::Kernel, cleanup_receipt).unwrap().unwrap();
    cleanup_call.invoke(&mut Script(|call: Call<'_>| {
                            assert_eq!(call.phase, Phase::Start);
                            Outcome::Complete(0)
                        }))
                .unwrap();
    queue.finish(cleanup_call).unwrap();
    queue.take_completion(Owner::Kernel, cleanup_receipt).unwrap().unwrap();
    assert!(queue.owned_bytes() > 0, "the original live ticket still owns its input and output");
    step(&queue, |call| {
        assert_eq!(call.phase, Phase::Poll);
        assert_eq!(*call.ticket, 91);
        *call.ticket = 0;
        Outcome::Complete(0)
    });
    queue.take_completion(Owner::Kernel, old_receipt).unwrap().unwrap();
    // The last acknowledged old job may retire its released backing. A
    // continuation must also be able to establish the next backing safely.
    let next = queue.test_access(|core| core.submit(&subject, Owner::Kernel, work(0))).unwrap();
    queue.discard_completion(Owner::Kernel, next.into_parts().1).unwrap();
    let extra = invalidate_jobs(&queue, &owner);
    assert!(extra.is_empty());
    drop(subject);
    settle(&queue, &owner, deferred);
}

#[test]
fn callback_borrows_the_original_subject_across_start_poll_and_cancel()
{
    let queue = Queue::new();
    let (owner, subject, publication) = admitted(&queue, Owner::Kernel);
    let (_, receipt) = publication.into_parts();
    let scope = subject.scope().clone();
    let root = scope.root().unwrap();
    let mut dispatch = queue.dispatch_completion(Owner::Kernel, receipt).unwrap().unwrap();
    dispatch.invoke(&mut Script(|call: Call<'_>| {
                        assert_eq!(call.owner,
                                   Owner::Kernel,
                                   "delivery policy is not subject authority");
                        assert!(call.subject.scope().same_scope(&scope));
                        *call.ticket = 91;
                        Outcome::Pending
                    }))
            .unwrap();
    queue.finish(dispatch).unwrap();
    let mut dispatch = queue.dispatch_completion(Owner::Kernel, receipt).unwrap().unwrap();
    dispatch.invoke(&mut Script(|call: Call<'_>| {
                        assert_eq!(call.phase, Phase::Poll);
                        assert!(call.subject.scope().same_scope(&scope));
                        Outcome::Pending
                    }))
            .unwrap();
    queue.finish(dispatch).unwrap();
    queue.cancel_completion(Owner::Kernel, receipt).unwrap();
    let mut dispatch = queue.dispatch_completion(Owner::Kernel, receipt).unwrap().unwrap();
    dispatch.invoke(&mut Script(|call: Call<'_>| {
                        assert_eq!(call.phase, Phase::Cancel);
                        assert!(call.subject.scope().same_scope(&scope));
                        assert!(owner.contains(root));
                        *call.ticket = 0;
                        Outcome::Failed(mac_abi::OPERATION_CANCELLED)
                    }))
            .unwrap();
    queue.finish(dispatch).unwrap();
    queue.take_completion(Owner::Kernel, receipt).unwrap().unwrap();
    drop(subject);
    assert!(!owner.contains(root));
    assert!(scope.admitting().is_err());
}

#[test]
fn native_jobs_grow_past_projection_limit_and_owner_discard_drains_every_pending_ack()
{
    let queue = Queue::new();
    let mut held = Vec::new();
    let mut ids = Vec::new();
    for _ in 0..160
    {
        ids.push(queue.submit(OWNER, Work::new(1, CLASS, 7, Vec::new(), 0).unwrap()).unwrap());
    }
    for _ in 0..160
    {
        let mut dispatch = queue.next_dispatch().unwrap();
        dispatch.invoke(&mut Script(|call: Call<'_>| {
                            *call.ticket = 73;
                            Outcome::Pending
                        }))
                .unwrap();
        held.push(dispatch);
    }
    for _ in 0..160
    {
        ids.push(queue.submit(OWNER, Work::new(1, CLASS, 7, Vec::new(), 0).unwrap()).unwrap());
    }
    assert!(ids.iter().all(|&id| id > u64::from(u32::MAX)));
    let capacity = queue.test_access(|core| core.ready.capacity());
    drop(held); // All actual pending invocations return into their origin queue.
    assert_eq!(queue.test_access(|core| core.ready.capacity()), capacity);
    queue.discard_owner(OWNER); // No fixed-size collection, no cleanup allocation.
    let mut acknowledged = 0;
    while let Some(mut dispatch) = queue.next_dispatch()
    {
        dispatch.invoke(&mut Script(|call: Call<'_>| {
                            assert_eq!(call.phase, Phase::Cancel);
                            assert_eq!(*call.ticket, 73);
                            *call.ticket = 0;
                            acknowledged += 1;
                            Outcome::Failed(mac_abi::OPERATION_CANCELLED)
                        }))
                .unwrap();
        queue.finish(dispatch).unwrap();
    }
    assert_eq!(acknowledged, 160, "unsent jobs must not contact the provider");
    assert_eq!(queue.test_access(|core| core.records.len()), 0);
    assert_eq!(queue.owned_bytes(), 0);
    for id in ids
    {
        assert_eq!(queue.pending(OWNER, id), Err(NOT_FOUND));
    }
}

#[test]
fn native_completion_notifications_have_room_for_every_admitted_return_guard()
{
    let queue = Queue::new();
    let mut ids = Vec::new();
    let mut held = Vec::new();
    for _ in 0..150
    {
        ids.push(queue.submit(OWNER, Work::new(1, CLASS, 7, Vec::new(), 0).unwrap()).unwrap());
    }
    for _ in 0..150
    {
        let mut dispatch = queue.next_dispatch().unwrap();
        dispatch.invoke(&mut Script(|_: Call<'_>| Outcome::Complete(0))).unwrap();
        held.push(dispatch);
    }
    for _ in 0..150
    {
        ids.push(queue.submit(OWNER, Work::new(1, CLASS, 7, Vec::new(), 0).unwrap()).unwrap());
    }
    let capacity = queue.test_access(|core| core.notices.capacity());
    drop(held);
    while let Some(mut dispatch) = queue.next_dispatch()
    {
        dispatch.invoke(&mut Script(|_: Call<'_>| Outcome::Complete(0))).unwrap();
        drop(dispatch); // The RAII path must enqueue the notification itself.
    }
    assert_eq!(queue.test_access(|core| core.notices.capacity()), capacity);
    let mut notices = Vec::new();
    while let Some(notice) = queue.take_notice()
    {
        notices.push(notice.job);
    }
    notices.sort_unstable();
    let mut expected = ids.clone();
    expected.sort_unstable();
    assert_eq!(notices, expected);
    for id in ids
    {
        assert!(queue.take(OWNER, id).unwrap().unwrap().result.unwrap().is_empty());
    }
    assert_eq!(queue.test_access(|core| core.records.len()), 0);
}
