//! Opt-in userspace runtime integration over native readiness. UI futures stay
//! on their owning thread; only a task's wake flag crosses threads.
#![forbid(unsafe_code)]
use std::{cell::{Cell, RefCell},
          future::Future,
          pin::Pin,
          rc::{Rc, Weak},
          sync::{Arc,
                 atomic::{AtomicBool, Ordering}},
          task::{Context, Poll, Wake, Waker}};

pub use mac_toolbox::abi::readiness::{CHILD, CLOSED, DEADLINE, INVALID, PIPE_READ, PIPE_WRITE,
                                      READY, Source, TIMER};
use mac_toolbox::{Error, Event, PointerPosition, System, abi::readiness as wire};
pub type LocalFuture = Pin<Box<dyn Future<Output = ()> + 'static>>;

/// Durable same-process wake right. This is a thread identity, never a user
/// pointer or callback. It does not make its executor or UI receivers Send.
#[derive(Clone, Copy)]
pub struct Notifier
{
    client: mac_toolbox::Client<'static>,
    thread: u64
}
impl Notifier
{
    pub fn current(system: &System<'static>) -> Result<Self, Error>
    {
        let mut call = wire::Call { operation: wire::CURRENT,
                                    ..Default::default() };
        system.client().readiness(&mut call)?;
        Ok(Self { client: system.client(),
                  thread: call.thread })
    }

    pub fn notify(self) -> Result<(), Error>
    {
        self.client.readiness(&mut wire::Call { operation: wire::NOTIFY,
                                                thread: self.thread,
                                                ..Default::default() })
    }
}
/// Source storage and event payloads are borrowed for this synchronous trap
/// only. On wake, recheck canonical states before delivering readiness.
pub fn wait(system: &System<'static>, sources: &mut [Source], timeout_ticks: u32)
            -> Result<(Event<'static>, PointerPosition), Error>
{
    Ok(system.events().wait_readiness(sources, timeout_ticks)?.into_parts())
}

/// A cancellation scope is local to the UI/runtime thread. Drop its owner to
/// cancel; task copies retain only the cancellation state, not its owner.
#[derive(Clone)]
pub struct Scope
{
    cancelled: Rc<Cell<bool>>,
    parent:    Option<Rc<Scope>>,
    notifier:  Notifier
}
pub struct ScopeOwner(Scope);
impl ScopeOwner
{
    pub fn new(notifier: Notifier) -> Self
    {
        Self(Scope { cancelled: Rc::new(Cell::new(false)),
                     parent: None,
                     notifier })
    }

    pub fn child(&self) -> Self
    {
        Self(Scope { cancelled: Rc::new(Cell::new(false)),
                     parent:    Some(Rc::new(self.0.clone())),
                     notifier:  self.0.notifier })
    }

    pub fn scope(&self) -> Scope
    {
        self.0.clone()
    }

    pub fn cancel(&self)
    {
        self.0.cancelled.set(true);
        let _ = self.0.notifier.notify();
    }
}
impl Drop for ScopeOwner
{
    fn drop(&mut self)
    {
        self.cancel();
    }
}
impl Scope
{
    pub fn cancel(&self)
    {
        self.cancelled.set(true);
        let _ = self.notifier.notify();
    }

    pub fn cancelled(&self) -> bool
    {
        self.cancelled.get() || self.parent.as_ref().is_some_and(|p| p.cancelled())
    }
}

/// Runtime boundary used by dispatch. An adapter may schedule local futures on
/// another runtime; it must honor scopes and never move UI objects to workers.
pub trait Runtime: Clone + 'static
{
    fn spawn(&self, scope: Scope, future: LocalFuture) -> Result<(), Error>;
}
struct WakeFlag
{
    ready:    AtomicBool,
    notifier: Notifier
}
impl Wake for WakeFlag
{
    fn wake(self: Arc<Self>)
    {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>)
    {
        self.ready.store(true, Ordering::Release);
        let _ = self.notifier.notify();
    }
}
struct Task
{
    future: LocalFuture,
    scope:  Scope,
    wake:   Arc<WakeFlag>
}
pub struct Engine
{
    driving:   Cell<bool>,
    tasks:     Rc<RefCell<Vec<Option<Task>>>>,
    interests: Rc<RefCell<Vec<Weak<RefCell<Interest>>>>>,
    notifier:  Notifier
}
impl Engine
{
    pub fn new(system: &System<'static>) -> Result<Self, Error>
    {
        Ok(Self { driving:   Cell::new(false),
                  tasks:     Rc::new(RefCell::new(Vec::new())),
                  interests: Rc::new(RefCell::new(Vec::new())),
                  notifier:  Notifier::current(system)? })
    }

    /// Await an owner-checked source without lending any I/O buffer to System.
    pub fn ready(&self, source: Source) -> Readiness
    {
        Readiness { interest:   Rc::new(RefCell::new(Interest { source,
                                                                result: None,
                                                                waker: None })),
                    registry:   self.interests.clone(),
                    registered: false }
    }

    pub fn sleep_until(&self, nanoseconds: u64) -> Readiness
    {
        self.ready(Source { kind: DEADLINE,
                            id: nanoseconds,
                            ..Default::default() })
    }

    /// Integrate all task interests with the app's UI/source wait. Readiness
    /// resumes the waiting future directly, independently of its handler mailbox.
    pub fn wait(&self, system: &System<'static>, sources: &mut Vec<Source>, timeout_ticks: u32)
                -> Result<(Event<'static>, PointerPosition), Error>
    {
        if self.driving.get()
        {
            return Err(Error::ServiceBusy);
        }
        let interests: Vec<_> = {
            let mut list = self.interests.borrow_mut();
            list.retain(|w| w.strong_count() != 0);
            list.iter().filter_map(Weak::upgrade).filter(|i| i.borrow().result.is_none()).collect()
        };
        let external = sources.len();
        sources.try_reserve(interests.len()).map_err(|_| Error::NoMemory)?;
        sources.extend(interests.iter().map(|i| i.borrow().source));
        let result = wait(system, sources, if self.has_ready() { 0 } else { timeout_ticks });
        if result.is_ok()
        {
            for (source, interest) in sources[external..].iter().zip(&interests)
            {
                if source.ready != 0
                {
                    let waker = {
                        let mut i = interest.borrow_mut();
                        i.result = Some(source.ready);
                        i.waker.take()
                    };
                    if let Some(waker) = waker
                    {
                        waker.wake();
                    }
                }
            }
        }
        sources.truncate(external);
        result
    }

    pub fn spawner(&self) -> Spawner
    {
        Spawner { tasks:    Rc::downgrade(&self.tasks),
                  notifier: self.notifier }
    }

    pub fn notifier(&self) -> Notifier
    {
        self.notifier
    }

    /// One bounded scheduling turn. Never hold task storage or a receiver borrow
    /// while polling user code. Self-wakes schedule a later turn, not a spin loop.
    pub fn turn(&self) -> Result<(), Error>
    {
        if self.driving.replace(true)
        {
            return Err(Error::ServiceBusy);
        }
        struct Reset<'a>(&'a Cell<bool>);
        impl Drop for Reset<'_>
        {
            fn drop(&mut self)
            {
                self.0.set(false);
            }
        }
        let _reset = Reset(&self.driving);
        let length = self.tasks.borrow().len();
        for index in 0..length
        {
            let task = {
                let mut tasks = self.tasks.borrow_mut();
                if tasks[index].as_ref().is_none_or(|t| {
                                            !t.scope.cancelled()
                                            && !t.wake.ready.swap(false, Ordering::AcqRel)
                                        })
                {
                    continue;
                }
                tasks[index].take()
            };
            let Some(mut task) = task
            else
            {
                continue;
            };
            if task.scope.cancelled()
            {
                continue;
            }
            let waker = Waker::from(task.wake.clone());
            let pending = task.future.as_mut().poll(&mut Context::from_waker(&waker)).is_pending();
            if pending && !task.scope.cancelled()
            {
                self.tasks.borrow_mut()[index] = Some(task);
            }
        }
        self.tasks.borrow_mut().retain(Option::is_some);
        Ok(())
    }

    pub fn has_ready(&self) -> bool
    {
        self.tasks
            .borrow()
            .iter()
            .flatten()
            .any(|t| t.scope.cancelled() || t.wake.ready.load(Ordering::Acquire))
    }
}
/// Weak scheduling handle: pending handlers cannot keep their engine alive.
#[derive(Clone)]
pub struct Spawner
{
    tasks:    Weak<RefCell<Vec<Option<Task>>>>,
    notifier: Notifier
}
impl Runtime for Spawner
{
    fn spawn(&self, scope: Scope, future: LocalFuture) -> Result<(), Error>
    {
        if scope.cancelled()
        {
            return Ok(());
        }
        let task = Task { future,
                          scope,
                          wake: Arc::new(WakeFlag { ready:    AtomicBool::new(true),
                                                    notifier: self.notifier }) };
        let storage = self.tasks.upgrade().ok_or(Error::NotFound)?;
        let mut tasks = storage.borrow_mut();
        // A slot temporarily removed during poll cannot be reused by a nested spawn.
        tasks.try_reserve(1).map_err(|_| Error::NoMemory)?;
        let index = tasks.len();
        tasks.push(Some(task));
        drop(tasks);
        if let Err(error) = self.notifier.notify()
        {
            let task = storage.borrow_mut()[index].take();
            drop(task);
            return Err(error);
        }
        Ok(())
    }
}


struct Interest
{
    source: Source,
    result: Option<u32>,
    waker:  Option<Waker>
}
#[must_use = "readiness futures must be awaited or polled by a runtime"]
pub struct Readiness
{
    interest:   Rc<RefCell<Interest>>,
    registry:   Rc<RefCell<Vec<Weak<RefCell<Interest>>>>>,
    registered: bool
}
impl Future for Readiness
{
    type Output = Result<u32, Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output>
    {
        let result = self.interest.borrow_mut().result.take();
        if let Some(flags) = result
        {
            return Poll::Ready(if flags & INVALID != 0
                               {
                                   Err(if self.interest.borrow().source.kind == TIMER
                                       {
                                           Error::Cancelled
                                       }
                                       else
                                       {
                                           Error::NotFound
                                       })
                               }
                               else
                               {
                                   Ok(flags)
                               });
        }
        if !self.registered
        {
            {
                let mut list = self.registry.borrow_mut();
                if list.try_reserve(1).is_err()
                {
                    return Poll::Ready(Err(Error::NoMemory));
                }
                list.push(Rc::downgrade(&self.interest));
            }
            self.registered = true;
        }
        self.interest.borrow_mut().waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

pub use mac_toolbox::events::TimeTask;
