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
    assert_eq!(first & 0xffff, next & 0xffff);
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
    step(&q, |_| Outcome::Complete(0));
    q.attach_waiter(Owner::Kernel, id, 42).unwrap();
    assert!(!q.pending_waiter(42, id).unwrap());
    assert_eq!(q.completed_waiter(Owner::Kernel, id).unwrap(), Some(42));
    q.take(Owner::Kernel, id).unwrap().unwrap();
    assert_eq!(q.pending_waiter(42, id), Err(NOT_FOUND));
    let next = q.submit(Owner::Kernel, work(8)).unwrap();
    assert_eq!(id & 0xffff, next & 0xffff);
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
