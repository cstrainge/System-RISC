//! A deadline/device observation may deschedule the CPU. Completion published
//! during that observation wins over poisoning a successfully completed queue.
#![forbid(unsafe_code)]

#[derive(Debug, PartialEq, Eq)]
pub enum Poll<E>
{
    Pending,
    Ready,
    Failed(E)
}

pub fn poll<E>(expected: u16, mut used: impl FnMut() -> u16, check: impl FnOnce() -> Option<E>)
               -> Poll<E>
{
    if used() == expected
    {
        return Poll::Ready;
    }
    let Some(error) = check()
    else
    {
        return Poll::Pending;
    };
    if used() == expected { Poll::Ready } else { Poll::Failed(error) }
}

#[cfg(test)]
mod tests
{
    use core::cell::Cell;

    use super::*;
    #[test]
    fn completion_during_descheduling_prevents_false_timeout_and_queue_poisoning()
    {
        let used = Cell::new(605);
        let reads = Cell::new(0);
        let result = poll(606,
                          || {
                              reads.set(reads.get() + 1);
                              used.get()
                          },
                          || {
                              // The vCPU was delayed in its clock/MMIO check while DMA completed.
                              used.set(606);
                              Some("deadline expired")
                          });
        assert_eq!(result, Poll::Ready);
        assert_eq!(reads.get(), 2);
    }
    #[test]
    fn ready_path_never_reads_device_or_clock_and_pending_does_no_extra_ring_read()
    {
        assert_eq!(poll(7, || 7, || -> Option<()> { panic!("unnecessary device/clock read") }),
                   Poll::Ready);
        let reads = Cell::new(0);
        assert_eq!(poll(7,
                        || {
                            reads.set(reads.get() + 1);
                            6
                        },
                        || None::<()>),
                   Poll::Pending);
        assert_eq!(reads.get(), 1);
    }
    #[test]
    fn actual_failure_retains_its_reason_and_different_or_wrapped_indices_cannot_ack()
    {
        assert_eq!(poll(7, || 6, || Some("device failed")), Poll::Failed("device failed"));
        assert_eq!(poll(0, || u16::MAX, || Some("deadline expired")),
                   Poll::Failed("deadline expired"));
        let used = Cell::new(u16::MAX);
        assert_eq!(poll(0,
                        || used.get(),
                        || {
                            used.set(0);
                            Some("late")
                        }),
                   Poll::Ready);
    }
}
