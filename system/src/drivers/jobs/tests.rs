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
fn step(queue: &mut Queue, action: impl FnMut(Call<'_>) -> Outcome) -> Option<CompletionNotice>
{
    let mut dispatch = queue.next_dispatch().unwrap();
    dispatch.invoke(&mut Script(action));
    queue.finish(dispatch).unwrap()
}
#[test]
fn completion_owns_result_and_retires_generation_with_separate_owner_checks()
{
    let mut q = Queue::new();
    let first = q.submit(OWNER, work(8)).unwrap();
    assert_eq!(q.take(Owner::Thread(43), first).unwrap_err(), ACCESS_DENIED);
    assert!(q.take(OWNER, first).unwrap().is_none());
    assert_eq!(step(&mut q, |call| {
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
    let mut other = Queue::new();
    let foreign = other.submit(OWNER, work(8)).unwrap();
    assert_ne!(foreign, next);
    assert_eq!(q.take(OWNER, foreign).unwrap_err(), NOT_FOUND);
    q.discard(OWNER, next).unwrap();
    other.discard(OWNER, foreign).unwrap();
}

#[test]
fn resident_result_survives_waiter_exit_and_delivers_an_acknowledged_mutation()
{
    let mut q = Queue::new();
    let id = q.submit(Owner::Kernel, work(8)).unwrap();
    q.attach_waiter(Owner::Kernel, id, 42).unwrap();
    assert!(q.pending_waiter(42, id).unwrap());
    assert_eq!(q.pending_waiter(43, id), Err(ACCESS_DENIED));
    assert_eq!(q.attach_waiter(Owner::Thread(42), id, 42), Err(ACCESS_DENIED));
    assert_eq!(q.attach_waiter(Owner::Kernel, id, 43), Err(SERVICE_BUSY));
    step(&mut q, |call| {
        *call.ticket = 91;
        Outcome::Pending
    });
    q.discard_owner(Owner::Thread(42));
    assert_eq!(q.pending_waiter(42, id), Err(ACCESS_DENIED));
    assert!(q.pending(Owner::Kernel, id).unwrap());
    let notice = step(&mut q, |call| {
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
    let mut q = Queue::new();
    let id = q.submit(Owner::Kernel, work(8)).unwrap();
    step(&mut q, |_| Outcome::Complete(0));
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
    let mut q = Queue::new();
    let id = q.submit(Owner::Kernel, work(8)).unwrap();
    assert_eq!(q.attach_waiter(Owner::Kernel, id, 1), Err(BAD_ARGUMENT));
    q.attach_waiter(Owner::Kernel, id, 42).unwrap();
    q.detach_waiter(42, id).unwrap();
    q.attach_waiter(Owner::Kernel, id, 43).unwrap();
    step(&mut q, |call| {
        *call.ticket = 92;
        Outcome::Pending
    });
    q.cancel(Owner::Kernel, id).unwrap();
    let notice = step(&mut q, |call| {
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
    let mut q = Queue::new();
    let id = q.submit(OWNER, work(8)).unwrap();
    q.cancel(OWNER, id).unwrap();
    q.cancel(OWNER, id).unwrap();
    assert!(!q.has_ready());
    assert!(q.next_dispatch().is_none());
    let result = q.take(OWNER, id).unwrap().unwrap();
    assert_eq!(result.result.unwrap_err(), Failure::Cancelled);
    assert!(result.cancellation_requested);
    assert_eq!(q.owned_bytes, 0);
}
#[test]
fn pending_cancellation_drains_actual_result_without_claiming_mutation_was_undone()
{
    let mut q = Queue::new();
    let id = q.submit(OWNER, work(8)).unwrap();
    assert_eq!(step(&mut q, |call| {
                   *call.ticket = 91;
                   Outcome::Pending
               }),
               None);
    q.cancel(OWNER, id).unwrap();
    assert_eq!(step(&mut q, |call| {
                   assert_eq!(call.phase, Phase::Cancel);
                   assert_eq!(*call.ticket, 91);
                   assert!(call.input.is_empty());
                   Outcome::Pending
               }),
               None);
    assert!(q.take(OWNER, id).unwrap().is_none());
    step(&mut q, |call| {
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
    let mut q = Queue::new();
    let id = q.submit(OWNER, work(8)).unwrap();
    step(&mut q, |call| {
        *call.ticket = 99;
        Outcome::Pending
    });
    let footprint = q.owned_bytes;
    q.discard_owner(OWNER);
    assert_eq!(q.owned_bytes, footprint);
    assert!(q.has_ready());
    step(&mut q, |call| {
        assert_eq!(call.phase, Phase::Cancel);
        Outcome::Pending
    });
    assert_eq!(q.owned_bytes, footprint);
    step(&mut q, |call| {
        assert_eq!(*call.ticket, 99);
        *call.ticket = 0;
        Outcome::Complete(0)
    });
    assert_eq!(q.owned_bytes, 0);
    assert!(!q.has_ready());
    assert_eq!(q.take(OWNER, id).unwrap_err(), NOT_FOUND);
}
#[test]
fn cancellation_during_callback_is_applied_to_next_poll_and_completed_result()
{
    let mut q = Queue::new();
    let id = q.submit(OWNER, work(8)).unwrap();
    let mut dispatch = q.next_dispatch().unwrap();
    q.cancel(OWNER, id).unwrap();
    dispatch.invoke(&mut Script(|call: Call<'_>| {
                        *call.ticket = 100;
                        Outcome::Pending
                    }));
    q.finish(dispatch).unwrap();
    step(&mut q, |call| {
        assert_eq!(call.phase, Phase::Cancel);
        *call.ticket = 0;
        Outcome::Complete(0)
    });
    assert!(q.take(OWNER, id).unwrap().unwrap().cancellation_requested);
}
#[test]
fn pending_jobs_are_fair_and_keep_borrowed_input_out_of_polls()
{
    let mut q = Queue::new();
    let first = q.submit(OWNER, work(8)).unwrap();
    let second = q.submit(OWNER, work(8)).unwrap();
    step(&mut q, |call| {
        assert_eq!(call.phase, Phase::Start);
        *call.ticket = 12;
        Outcome::Pending
    });
    assert_eq!(step(&mut q, |call| {
                   assert_eq!(call.phase, Phase::Start);
                   Outcome::Complete(0)
               }),
               Some(CompletionNotice { owner:  OWNER,
                                       waiter: Some(42),
                                       job:    second }));
    assert_eq!(step(&mut q, |call| {
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
    let mut q = Queue::new();
    let id = q.submit(OWNER, work(8)).unwrap();
    step(&mut q, |call| {
        *call.ticket = 13;
        Outcome::Pending
    });
    step(&mut q, |call| {
        assert_eq!(call.phase, Phase::Poll);
        Outcome::Failed(SERVICE_BUSY)
    });
    step(&mut q, |call| {
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
    let mut q = Queue::new();
    let id = q.submit(OWNER, work(8)).unwrap();
    step(&mut q, |call| {
        *call.ticket = 14;
        Outcome::Complete(0)
    });
    assert!(q.take(OWNER, id).unwrap().is_none());
    step(&mut q, |call| {
        assert_eq!(call.phase, Phase::Cancel);
        *call.ticket = 0;
        Outcome::Complete(0)
    });
    assert_eq!(q.take(OWNER, id).unwrap().unwrap().result.unwrap_err(), Failure::Service(BAD_ABI));
}
#[test]
fn foreign_or_uninvoked_dispatch_is_returned_intact_not_dropped()
{
    let mut q = Queue::new();
    let id = q.submit(OWNER, work(8)).unwrap();
    let dispatch = q.next_dispatch().unwrap();
    let mut dispatch = q.finish(dispatch).unwrap_err();
    dispatch.invoke(&mut Script(|call: Call<'_>| {
                        *call.ticket = 15;
                        Outcome::Pending
                    }));
    let mut other = Queue::new();
    let dispatch = other.finish(dispatch).unwrap_err();
    assert_eq!(dispatch.work.ticket, 15);
    q.finish(dispatch).unwrap();
    step(&mut q, |call| {
        assert_eq!(*call.ticket, 15);
        *call.ticket = 0;
        Outcome::Complete(0)
    });
    q.take(OWNER, id).unwrap();
}
#[test]
fn memory_budget_fails_before_dispatch_and_returns_unsent_owned_input()
{
    let mut q = Queue::new();
    let id = q.submit(OWNER, Work::new(1, CLASS, 7, Vec::new(), MAX_OWNED_BYTES).unwrap()).unwrap();
    let error = q.submit(OWNER, work(8)).unwrap_err();
    assert_eq!(error.status, NO_MEMORY);
    assert_eq!(error.work.input, [1, 2, 3]);
    q.discard(OWNER, id).unwrap();
    assert_eq!(q.owned_bytes, 0);
    let id = q.submit(OWNER, error.work).unwrap();
    q.discard(OWNER, id).unwrap();
}

#[test]
fn admission_contention_before_start_retains_owned_work_and_can_be_cancelled_unsent()
{
    let mut q = Queue::new();
    let id = q.submit(OWNER, work(8)).unwrap();
    step(&mut q, |call| {
        assert_eq!(*call.ticket, 0);
        Outcome::Contended
    });
    assert!(q.pending(OWNER, id).unwrap());
    step(&mut q, |call| {
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
    let mut q = Queue::new();
    let id = q.submit(OWNER, work(8)).unwrap();
    let mut dispatch = q.next_dispatch().unwrap();
    q.cancel(OWNER, id).unwrap();
    dispatch.invoke(&mut Script(|_: Call<'_>| Outcome::Contended));
    q.finish(dispatch).unwrap();
    assert!(!q.has_ready());
    assert_eq!(q.take(OWNER, id).unwrap().unwrap().result.unwrap_err(), Failure::Cancelled);
}
