//! An adversarial dispatcher must retain, report and retry the exact refused
//! ticket. It cannot turn a terminal transport reply into a false ledger ACK.
use super::super::{tests::{admitted, invalidate_jobs, settle},
                   *};
struct Complete;
impl Broker for Complete
{
    fn invoke(&mut self, _: Call<'_>) -> Outcome
    {
        Outcome::Complete(0)
    }
}
#[test]
fn refused_ticket_is_reported_once_and_retry_never_acknowledges_a_foreign_publication()
{
    let first = Queue::new();
    let second = Queue::new();
    let (owner_a, subject_a, publication_a) = admitted(&first, Owner::Kernel);
    let (owner_b, subject_b, publication_b) = admitted(&second, Owner::Kernel);
    for queue in [&first, &second]
    {
        let mut dispatch = queue.next_dispatch().unwrap();
        dispatch.invoke(&mut Complete).unwrap();
        queue.finish(dispatch).unwrap();
    }
    let deferred_a = invalidate_jobs(&first, &owner_a);
    let deferred_b = invalidate_jobs(&second, &owner_b);
    let exchange = || {
        first.test_access(|a| {
                 second.test_access(|b| {
                           assert_eq!(a.records.retiring.len(), 1);
                           assert_eq!(b.records.retiring.len(), 1);
                           core::mem::swap(&mut a.records.retiring[0].ticket,
                                           &mut b.records.retiring[0].ticket);
                       })
             })
    };
    exchange(); // Inject a real, foreign capability; never manufacture a ticket.
    let (_, receipt_a) = publication_a.into_parts();
    first.take_completion(Owner::Kernel, receipt_a).unwrap().unwrap();
    assert!(owner_a.summary().pending > 0 && owner_b.summary().pending > 0);
    let failure = first.take_return_failure().unwrap();
    assert_eq!(failure.job, publication_a.id());
    assert!(matches!(failure.cause, ReturnCause::CleanupRefused(_)));
    let message = alloc::format!("{}", failure.cause);
    assert!(message.contains("cleanup refused") && message.contains("exact cleanup ticket"));
    assert!(first.take_return_failure().is_none(),
            "unchanged failure must not flood the event pump");
    assert_eq!(first.pending(Owner::Kernel, publication_a.id()), Err(NOT_FOUND));
    exchange();
    first.test_access(|core| core.records.remove(publication_a.id()));
    second.discard_completion(Owner::Kernel, publication_b.into_parts().1).unwrap();
    drop(subject_a);
    drop(subject_b);
    settle(&first, &owner_a, deferred_a);
    settle(&second, &owner_b, deferred_b);
}
