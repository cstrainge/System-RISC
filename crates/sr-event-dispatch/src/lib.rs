//! Weak event sinks with one ordered queue per handler. A source can live in a
//! control wrapper or a target-ID router; both use exactly the same mailbox.
#![forbid(unsafe_code)]
use std::{cell::RefCell,
          collections::VecDeque,
          rc::{Rc, Weak}};

use mac_toolbox::Error;
use sr_event_async::{LocalFuture, Runtime, Scope};
trait Sink<E>
{
    fn enqueue(&self, event: E) -> Result<(), Error>;
    fn alive(&self) -> bool;
}
struct Mailbox<T, E, R, F>
{
    target:  Weak<RefCell<T>>,
    queue:   RefCell<VecDeque<E>>,
    handler: RefCell<F>,
    runtime: R,
    scope:   Scope,
    busy:    std::cell::Cell<bool>
}
// Construct before spawning, so dropping an unpolled operation also releases
// its accepted queue. Pending payloads are dropped without a mailbox borrow.
struct Busy<T, E, R, F>(Rc<Mailbox<T, E, R, F>>);
impl<T, E, R, F> Drop for Busy<T, E, R, F>
{
    fn drop(&mut self)
    {
        self.0.busy.set(false);
        let pending = {
            let mut queue = self.0.queue.borrow_mut();
            if queue.is_empty() { None } else { Some(std::mem::take(&mut *queue)) }
        };
        drop(pending);
    }
}
impl<T: 'static, E: 'static, R: Runtime, F: FnMut(&mut T, E) -> LocalFuture + 'static>
    Mailbox<T, E, R, F>
{
    fn submit(self: &Rc<Self>, event: E) -> Result<(), Error>
    {
        if !self.alive()
        {
            return Ok(());
        }
        {
            let mut queue = self.queue.borrow_mut();
            queue.try_reserve(1).map_err(|_| Error::NoMemory)?;
            queue.push_back(event);
        }
        if self.busy.replace(true)
        {
            return Ok(());
        }
        let me = self.clone();
        let busy = Busy(me.clone());
        if let Err(error) =
            self.runtime.spawn(self.scope.clone(),
                               Box::pin(async move {
                                   let _busy = busy;
                                   loop
                                   {
                                       if !me.alive()
                                       {
                                           me.queue.borrow_mut().clear();
                                           break;
                                       }
                                       let Some(event) = me.queue.borrow_mut().pop_front()
                                       else
                                       {
                                           break;
                                       };
                                       // Only the synchronous factory borrows the target. The
                                       // returned future
                                       // owns its operation state and cannot borrow this RefMut
                                       // across await.
                                       let operation = {
                                           let Some(target) = me.target.upgrade()
                                           else
                                           {
                                               break;
                                           };
                                           let mut target = target.borrow_mut();
                                           (me.handler.borrow_mut())(&mut target, event)
                                       };
                                       operation.await;
                                       // Fairness is needed only when another delivery is queued.
                                       // A completed empty mailbox must not wake an idle runtime.
                                       if me.queue.borrow().is_empty()
                                       {
                                           break;
                                       }
                                       YieldOnce(false).await;
                                   }
                               }))
        {
            // The captured guard owns rollback, including an unpolled future.
            // Do not touch a new queue created by a reentrant payload destructor.
            return Err(error);
        }
        Ok(())
    }
}
// A local Rc wrapper lets erased sinks retain mailbox identity without owning T.
struct Recipient<T, E, R, F>(Rc<Mailbox<T, E, R, F>>);
impl<T: 'static, E: 'static, R: Runtime, F: FnMut(&mut T, E) -> LocalFuture + 'static> Sink<E>
    for Recipient<T, E, R, F>
{
    fn enqueue(&self, event: E) -> Result<(), Error>
    {
        self.0.submit(event)
    }

    fn alive(&self) -> bool
    {
        self.0.target.strong_count() != 0 && !self.0.scope.cancelled()
    }
}
impl<T, E, R, F> Mailbox<T, E, R, F>
{
    fn alive(&self) -> bool
    {
        self.target.strong_count() != 0 && !self.scope.cancelled()
    }
}
/// Clone to share this same queue across window, control and readiness sources.
/// Constructing another handler explicitly creates another independent queue.
pub struct Handler<E>(Rc<dyn Sink<E>>);
impl<E> Clone for Handler<E>
{
    fn clone(&self) -> Self
    {
        Self(self.0.clone())
    }
}
impl<E: 'static> Handler<E>
{
    pub fn new<T: 'static, R: Runtime, F: FnMut(&mut T, E) -> LocalFuture + 'static>(target:Weak<RefCell<T>>,
                                                                                     runtime: R,
                                                                                     scope: Scope,
                                                                                     handler: F)
                                                                                     -> Self
    {
        Self(Rc::new(Recipient(Rc::new(Mailbox { target,
                                                 runtime,
                                                 scope,
                                                 handler: RefCell::new(handler),
                                                 queue: RefCell::new(VecDeque::new()),
                                                 busy: std::cell::Cell::new(false) }))))
    }

    pub fn send(&self, event: E) -> Result<(), Error>
    {
        self.0.enqueue(event)
    }

    pub fn alive(&self) -> bool
    {
        self.0.alive()
    }
}
/// Source-local subscription. Owned by a control/window wrapper, with no
/// app-wide recipient registry. Its sole subscriber is held weakly through
/// Handler; dead targets are pruned even without a subsequent emission.
pub struct Source<E>
{
    subscriber: Option<Handler<E>>
}
impl<E> Default for Source<E>
{
    fn default() -> Self
    {
        Self { subscriber: None }
    }
}
impl<E: 'static> Source<E>
{
    pub fn on(&mut self, handler: Handler<E>)
    {
        self.subscriber = Some(handler);
    }

    pub fn emit(&mut self, event: E) -> Result<(), Error>
    {
        self.prune();
        if let Some(h) = &self.subscriber
        {
            h.send(event)?;
        }
        Ok(())
    }

    pub fn prune(&mut self)
    {
        if self.subscriber.as_ref().is_some_and(|h| !h.alive())
        {
            self.subscriber = None;
        }
    }
}
pub struct Router<K, E>
{
    routes: Vec<(K, Handler<E>)>
}
impl<K: Ord, E: 'static> Default for Router<K, E>
{
    fn default() -> Self
    {
        Self { routes: Vec::new() }
    }
}
impl<K: Ord, E: 'static> Router<K, E>
{
    pub fn on(&mut self, key: K, handler: Handler<E>) -> Result<(), Error>
    {
        match self.routes.binary_search_by(|(k, _)| k.cmp(&key))
        {
            Ok(index) => self.routes[index].1 = handler,
            Err(index) =>
            {
                self.routes.try_reserve(1).map_err(|_| Error::NoMemory)?;
                self.routes.insert(index, (key, handler));
            }
        }
        Ok(())
    }

    pub fn route(&mut self, key: &K, event: E) -> Result<bool, Error>
    {
        let Ok(index) = self.routes.binary_search_by(|(k, _)| k.cmp(key))
        else
        {
            return Ok(false);
        };
        if !self.routes[index].1.alive()
        {
            self.routes.remove(index);
            return Ok(false);
        }
        self.routes[index].1.send(event)?;
        Ok(true)
    }

    pub fn prune(&mut self)
    {
        self.routes.retain(|(_, h)| h.alive());
    }
}

/// Give other ready receivers a turn between queued deliveries. This is
/// scheduling runnable work, never periodic polling of a pending operation.
struct YieldOnce(bool);
impl std::future::Future for YieldOnce
{
    type Output = ();

    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>)
            -> std::task::Poll<()>
    {
        if self.0
        {
            std::task::Poll::Ready(())
        }
        else
        {
            self.0 = true;
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    }
}
