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

