//! One IRQ/preemption-audited lock boundary, including dispatch guard Drop.
use core::sync::atomic::{AtomicU8, Ordering};

use mac_object_table::shared::InvocationGate as _;
use spinning_top::Spinlock;

use super::*;
const READY: u8 = 1;
const NOTICE: u8 = 2;
const FAILURE: u8 = 4;
pub struct Queue
{
    core:   Spinlock<Core>,
    signal: AtomicU8
}
impl Default for Queue
{
    fn default() -> Self
    {
        Self::new()
    }
}
impl Queue
{
    pub const fn new() -> Self
    {
        Self { core:   Spinlock::new(Core::new()),
               signal: AtomicU8::new(0) }
    }

    fn access<R>(&self, operation: impl FnOnce(&mut Core) -> R) -> R
    {
        crate::toolbox::handles::InvocationGate::run(|| {
            let mut core = self.core.lock();
            let result = operation(&mut core);
            let signal = u8::from(!core.ready.is_empty()) * READY
                         | u8::from(!core.notices.is_empty()) * NOTICE
                         | u8::from(core.unreported != 0) * FAILURE;
            if self.signal.load(Ordering::Relaxed) != signal
            {
                self.signal.store(signal, Ordering::Release);
            }
            result
        })
    }

    pub fn has_ready(&self) -> bool
    {
        self.signal.load(Ordering::Acquire) != 0
    }

    pub fn submit(&self, owner: Owner, work: Work) -> Result<u32, SubmissionError>
    {
        self.access(|core| core.submit(owner, work))
    }

    pub fn pending(&self, owner: Owner, id: u32) -> Result<bool, Status>
    {
        self.access(|core| core.pending(owner, id))
    }

    pub fn attach_waiter(&self, owner: Owner, id: u32, thread: u64) -> Result<(), Status>
    {
        self.access(|core| core.attach_waiter(owner, id, thread))
    }

    pub fn pending_waiter(&self, thread: u64, id: u32) -> Result<bool, Status>
    {
        self.access(|core| core.pending_waiter(thread, id))
    }

    pub fn detach_waiter(&self, thread: u64, id: u32) -> Result<(), Status>
    {
        self.access(|core| core.detach_waiter(thread, id))
    }

    pub fn completed_waiter(&self, owner: Owner, id: u32) -> Result<Option<u64>, Status>
    {
        self.access(|core| core.completed_waiter(owner, id))
    }

    pub fn next_dispatch(&self) -> Option<Dispatch<'_>>
    {
        if self.signal.load(Ordering::Acquire) & READY == 0
        {
            return None;
        }
        self.access(|core| core.next_dispatch()).map(|node| {
                                                    Dispatch { queue:       self,
                                                               node:        Some(node),
                                                               in_callback: false }
                                                })
    }

    pub fn dispatch_for(&self, owner: Owner, id: u32) -> Result<Option<Dispatch<'_>>, Status>
    {
        self.access(|core| core.dispatch_for(owner, id)).map(|node| {
                                                            node.map(|node| {
                                                                    Dispatch { queue:       self,
                                                                               node:
                                                                                   Some(node),
                                                                               in_callback: false }
                                                                })
                                                        })
    }

    pub fn finish<'q>(&self, mut dispatch: Dispatch<'q>)
                      -> Result<Option<CompletionNotice>, Dispatch<'q>>
    {
        if !core::ptr::eq(self, dispatch.queue) || dispatch.in_callback
        {
            return Err(dispatch);
        }
        let Some(node) = dispatch.node.take()
        else
        {
            return Err(dispatch);
        };
        match self.access(|core| core.finish(node))
        {
            Ok(notice) => Ok(notice),
            Err(node) =>
            {
                dispatch.node = Some(node);
                Err(dispatch)
            }
        }
    }

    pub(super) fn return_owned(&self, node: Box<Node>, interrupted: bool)
    {
        self.access(|core| core.return_owned(node, interrupted));
    }

    pub fn take_notice(&self) -> Option<CompletionNotice>
    {
        if self.signal.load(Ordering::Acquire) & NOTICE == 0
        {
            return None;
        }
        self.access(Core::notice)
    }

    pub fn take_return_failure(&self) -> Option<ReturnFailure>
    {
        if self.signal.load(Ordering::Acquire) & FAILURE == 0
        {
            return None;
        }
        self.access(Core::failure)
    }

    pub fn take(&self, owner: Owner, id: u32) -> Result<Option<Completed>, Status>
    {
        self.access(|core| core.take(owner, id))
    }

    pub fn cancel(&self, owner: Owner, id: u32) -> Result<(), Status>
    {
        self.access(|core| core.cancel(owner, id))
    }

    pub fn discard(&self, owner: Owner, id: u32) -> Result<(), Status>
    {
        self.access(|core| core.discard(owner, id))
    }

    pub fn discard_owner(&self, owner: Owner)
    {
        self.access(|core| core.discard_owner(owner));
    }

    #[cfg(test)]
    pub(super) fn owned_bytes(&self) -> usize
    {
        self.access(|core| core.owned_bytes)
    }
}
