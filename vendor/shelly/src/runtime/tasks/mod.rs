
use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::BTreeMap,
    future::{Future, poll_fn},
    io,
    pin::Pin,
    ptr::NonNull,
    rc::{Rc, Weak},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};

use corosensei::{Coroutine, CoroutineResult, ScopedCoroutine, Yielder};
#[cfg(not(target_os = "systemrisc"))]
use corosensei::stack::DefaultStack as TaskStack;
#[cfg(target_os = "systemrisc")]
mod systemrisc;
#[cfg(target_os = "systemrisc")]
use systemrisc::TaskStack;

#[cfg(target_os = "systemrisc")]
const TASK_STACK_BYTES: usize = 8 * 1024 * 1024;
#[cfg(not(target_os = "systemrisc"))]
const TASK_STACK_BYTES: usize = 16 * 1024 * 1024;

use tokio::runtime::{Builder, Runtime};

use crate::language::interpreter::InterpreterError;
use crate::runtime::signals::take_interrupt;

#[derive(Clone, Debug)]
pub struct NativeFault
{
    pub kind: String,
    pub message: String,
    pub code: Option<i32>,
    pub file: &'static str,
    pub line: u32,
    pub column: u32,
    pub script_error: Option<u64>,
    pub cancelled: bool,
}

impl NativeFault
{
    #[track_caller]
    pub fn error(kind: &str, error: &(dyn std::error::Error + 'static)) -> Self
    {
        let mut fault = Self::new(kind, error.to_string());
        let mut current = Some(error);
        while let Some(error) = current
        {
            #[cfg(not(target_os = "systemrisc"))]
            if error
                .downcast_ref::<reqwest::Error>()
                .is_some_and(reqwest::Error::is_timeout)
            {
                fault.kind = "TimeoutError".into();
            }
            if let Some(script) = error.downcast_ref::<ScriptFault>()
            {
                fault.script_error = Some(script.0);
                break;
            }
            if let Some(error) = error.downcast_ref::<io::Error>()
                && let Some(script) = error
                    .get_ref()
                    .and_then(|error| error.downcast_ref::<ScriptFault>())
            {
                fault.script_error = Some(script.0);
                break;
            }
            current = error.source();
        }
        fault
    }
    #[track_caller]
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self
    {
        let source = std::panic::Location::caller();
        Self {
            kind: kind.into(),
            message: message.into(),
            code: None,
            file: source.file(),
            line: source.line(),
            column: source.column(),
            script_error: None,
            cancelled: false,
        }
    }
    #[track_caller]
    pub fn io(kind: &str, error: io::Error) -> Self
    {
        let mut fault = Self::error(kind, &error);
        fault.code = error.raw_os_error();
        fault.script_error = error
            .get_ref()
            .and_then(|error| error.downcast_ref::<ScriptFault>())
            .map(|error| error.0);
        fault
    }
}

#[derive(Debug)]
pub struct ScriptFault(pub u64);
impl std::fmt::Display for ScriptFault
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result
    {
        formatter.write_str("Script byte producer failed")
    }
}
impl std::error::Error for ScriptFault {}

// Native futures can carry owned Rust data, never an interpreter, Value, or function reference.
pub type NativeReply = Result<Box<dyn Any + Send>, NativeFault>;
pub type NativeFuture = Pin<Box<dyn Future<Output = NativeReply> + Send>>;

enum Suspension
{
    Wait(NativeFuture),
    Runnable,
}
type TaskYielder = Yielder<NativeReply, Suspension>;

thread_local! {
    static TASK_ID: Cell<u64> = const { Cell::new(0) };
    static RECEIVERS: RefCell<BTreeMap<usize, (u64, usize)>> = const { RefCell::new(BTreeMap::new()) };
    static CHILD: Cell<bool> = const { Cell::new(false) };
    static CURRENT: Cell<Option<NonNull<TaskYielder>>> = const { Cell::new(None) };
    static ENGINE: RefCell<Weak<Reactor>> = const { RefCell::new(Weak::new()) };
}

struct CurrentGuard(Option<NonNull<TaskYielder>>);

impl Drop for CurrentGuard
{
    fn drop(&mut self)
    {
        CURRENT.set(self.0);
    }
}

pub fn current_task() -> u64
{
    TASK_ID.get()
}

pub fn managed() -> bool
{
    CHILD.get()
}

pub fn active() -> bool
{
    CURRENT.get().is_some()
}
pub fn current_reactor() -> Option<Rc<Reactor>>
{
    ENGINE.with(|engine| engine.borrow().upgrade())
}

fn suspend(signal: Suspension) -> NativeReply
{
    let Some(yielder) = CURRENT.get()
    else
    {
        return Err(NativeFault::new(
            "UnsupportedSuspension",
            "No running VM task",
        ));
    };
    // The pointer is installed only inside ScopedCoroutine's closure. It is used on that same
    // thread and cleared while the dispatcher runs. ScopedCoroutine drives or unwinds the stack
    // before any borrowed closure data can expire; this pointer never escapes this module.
    let task = TASK_ID.get();
    let reply = unsafe { yielder.as_ref().suspend(signal) };
    TASK_ID.set(task);
    CHILD.set(task != 0);
    CURRENT.set(Some(yielder));
    reply
}

pub struct ReceiverGuard
{
    key: usize,
    task: u64,
}
impl Drop for ReceiverGuard
{
    fn drop(&mut self)
    {
        RECEIVERS.with(|receivers| {
            let mut receivers = receivers.borrow_mut();
            if let Some((owner, count)) = receivers.get_mut(&self.key)
                && *owner == self.task
            {
                *count -= 1;
                if *count == 0
                {
                    receivers.remove(&self.key);
                }
            }
        });
    }
}
pub fn check_write(key: usize) -> Result<(), NativeFault>
{
    RECEIVERS.with(|receivers| {
        if receivers
            .borrow()
            .get(&key)
            .is_some_and(|(owner, _)| *owner != TASK_ID.get())
        {
            Err(NativeFault::new(
                "ReceiverBusy",
                "Binding is pinned by another suspended receiver",
            ))
        }
        else
        {
            Ok(())
        }
    })
}
pub fn pin_receiver(key: usize) -> Result<ReceiverGuard, NativeFault>
{
    check_write(key)?;
    let task = TASK_ID.get();
    RECEIVERS.with(|receivers| {
        receivers
            .borrow_mut()
            .entry(key)
            .and_modify(|(_, count)| *count += 1)
            .or_insert((task, 1));
    });
    Ok(ReceiverGuard {
        key,
        task,
    })
}

pub fn wait(future: NativeFuture) -> NativeReply
{
    suspend(Suspension::Wait(future))
}
pub fn cooperate() -> Result<(), NativeFault>
{
    suspend(Suspension::Runnable).map(|_| ())
}

/// One reactor belongs to one interpreter thread and survives REPL submissions.
pub struct Reactor
{
    runtime: Runtime,
    running: RefCell<bool>,
    tasks: RefCell<BTreeMap<u64, Task>>,
    errors: RefCell<BTreeMap<u64, InterpreterError>>,
    next: Cell<u64>,
}

struct Task
{
    coroutine: Coroutine<NativeReply, Suspension, (), TaskStack>,
    future: Option<NativeFuture>,
    reply: Option<NativeReply>,
    cancel: Arc<AtomicBool>,
    cancelled_at: Option<Instant>,
}

impl Reactor
{
    pub fn new() -> io::Result<Rc<Self>>
    {
        Ok(Rc::new(Self {
            runtime: Builder::new_current_thread()
                .enable_all()
                .max_blocking_threads(4)
                .build()?,
            running: RefCell::new(false),
            tasks: RefCell::new(BTreeMap::new()),
            errors: RefCell::new(BTreeMap::new()),
            next: Cell::new(1),
        }))
    }

    pub fn store_error(&self, error: InterpreterError) -> u64
    {
        let id = self.next.get();
        self.next.set(id + 1);
        self.errors.borrow_mut().insert(id, error);
        id
    }
    pub fn take_error(&self, id: u64) -> Option<InterpreterError>
    {
        self.errors.borrow_mut().remove(&id)
    }

    pub fn abort_tasks(&self)
    {
        for task in self.tasks.borrow_mut().values_mut()
        {
            task.cancel.store(true, Ordering::Release);
            task.cancelled_at = Some(Instant::now() - Duration::from_secs(5));
        }
    }

    pub fn cancelled_tasks_pending(&self) -> bool
    {
        self.tasks
            .borrow()
            .values()
            .any(|task| task.cancel.load(Ordering::Acquire))
    }

    pub fn available_tasks(&self) -> usize
    { 32usize.saturating_sub(self.tasks.borrow().len()) }

    pub fn spawn(
        self: &Rc<Self>,
        cancel: Arc<AtomicBool>,
        operation: impl FnOnce() + 'static,
    ) -> io::Result<u64>
    {
        if self.tasks.borrow().len() >= 32
        {
            return Err(io::Error::other("VM task limit exceeded"));
        }
        let stack = TaskStack::new(TASK_STACK_BYTES)?;
        let id = self.next.get();
        self.next.set(id + 1);
        let engine = Rc::downgrade(self);
        let operation: Box<dyn FnOnce()> = Box::new(operation);
        let coroutine = Coroutine::with_stack(stack, move |yielder, _| {
            ENGINE.with(|current| *current.borrow_mut() = engine);
            let previous = CURRENT.replace(Some(NonNull::from(yielder)));
            let _guard = CurrentGuard(previous);
            TASK_ID.set(id);
            let previous_child = CHILD.replace(true);
            operation();
            CHILD.set(previous_child);
        });
        self.tasks.borrow_mut().insert(
            id,
            Task {
                coroutine,
                future: None,
                reply: Some(Ok(Box::new(()))),
                cancel,
                cancelled_at: None,
            },
        );
        Ok(id)
    }

    fn poll_tasks(&self, context: &mut Context<'_>)
    {
        let ids: Vec<_> = self.tasks.borrow().keys().copied().collect();
        for id in ids
        {
            // No RefCell guard survives a script callback or a coroutine suspension.
            let Some(mut task) = self.tasks.borrow_mut().remove(&id)
            else
            {
                continue;
            };
            if task.cancel.load(Ordering::Acquire)
            {
                let since = *task.cancelled_at.get_or_insert_with(Instant::now);
                task.future.take();
                let mut fault = NativeFault::new(
                    if since.elapsed() < Duration::from_secs(5)
                    {
                        "Interrupted"
                    }
                    else
                    {
                        "CleanupAbort"
                    },
                    "Byte source cancelled",
                );
                fault.cancelled = true;
                task.reply = Some(Err(fault));
            }
            if let Some(future) = &mut task.future
                && let Poll::Ready(reply) = future.as_mut().poll(context)
            {
                task.future.take();
                task.reply = Some(reply);
            }
            if let Some(reply) = task.reply.take()
            {
                let step = task.coroutine.resume(reply);
                CURRENT.set(None);
                CHILD.set(false);
                TASK_ID.set(0);
                match step
                {
                    CoroutineResult::Return(()) =>
                    {
                        CHILD.set(false);
                        continue;
                    }
                    CoroutineResult::Yield(Suspension::Wait(future)) => task.future = Some(future),
                    CoroutineResult::Yield(Suspension::Runnable) =>
                    {
                        task.reply = Some(Ok(Box::new(())))
                    }
                }
                // The yielded future needs its first poll to register a wakeup.
                context.waker().wake_by_ref();
            }
            self.tasks.borrow_mut().insert(id, task);
        }
    }

    pub fn run<T>(self: &Rc<Self>, operation: impl FnOnce() -> T) -> io::Result<T>
    {
        let stack = TaskStack::new(TASK_STACK_BYTES)?;
        let task = ScopedCoroutine::with_stack(stack, |yielder, _| {
            TASK_ID.set(0);
            ENGINE.with(|engine| *engine.borrow_mut() = Rc::downgrade(self));
            let previous = CURRENT.replace(Some(NonNull::from(yielder)));
            let _guard = CurrentGuard(previous);
            operation()
        });
        Ok(task.scope(|mut task| {
            let mut reply: NativeReply = Ok(Box::new(()));
            loop
            {
                let step = task.as_mut().resume(reply);
                CURRENT.set(None);
                match step
                {
                    CoroutineResult::Return(value) =>
                    {
                        // Operation tasks cannot survive their root submission. Cancellation
                        // resumes script unwinding, allowing finally to run under its budget.
                        for task in self.tasks.borrow().values()
                        {
                            task.cancel.store(true, Ordering::Release);
                        }
                        self.runtime.block_on(async {
                            let deadline = Instant::now() + Duration::from_secs(5);
                            while !self.tasks.borrow().is_empty() && Instant::now() < deadline
                            {
                                poll_fn(|context| {
                                    self.poll_tasks(context);
                                    Poll::Ready(())
                                })
                                .await;
                                tokio::time::sleep(Duration::from_millis(1)).await;
                            }
                        });
                        self.tasks.borrow_mut().clear();
                        self.errors.borrow_mut().clear();
                        // Abort handles used by native clients/servers also need a reactor
                        // turn to drop their owned socket futures before the next REPL input.
                        self.runtime.block_on(tokio::task::yield_now());
                        CHILD.set(false);
                        return value;
                    }
                    CoroutineResult::Yield(Suspension::Runnable) =>
                    {
                        self.runtime.block_on(async {
                            poll_fn(|context| {
                                self.poll_tasks(context);
                                Poll::Ready(())
                            })
                            .await;
                            tokio::task::yield_now().await;
                        });
                        reply = Ok(Box::new(()));
                    }
                    CoroutineResult::Yield(Suspension::Wait(mut future)) =>
                    {
                        *self.running.borrow_mut() = true;
                        reply = self.runtime.block_on(async {
                            let mut tick = tokio::time::interval(Duration::from_millis(10));
                            loop
                            {
                                let completed = poll_fn(|context| {
                                    self.poll_tasks(context);
                                    match future.as_mut().poll(context)
                                    {
                                        Poll::Ready(reply) => Poll::Ready(Some(reply)),
                                        Poll::Pending => match tick.poll_tick(context)
                                        {
                                            Poll::Ready(_) => Poll::Ready(None),
                                            Poll::Pending => Poll::Pending,
                                        },
                                    }
                                })
                                .await;
                                if let Some(reply) = completed
                                {
                                    break reply;
                                }
                                if take_interrupt()
                                {
                                    break Err(NativeFault::new(
                                        "Interrupted",
                                        "Operation interrupted",
                                    ));
                                }
                            }
                        });
                        *self.running.borrow_mut() = false;
                    }
                }
            }
        }))
    }
}
