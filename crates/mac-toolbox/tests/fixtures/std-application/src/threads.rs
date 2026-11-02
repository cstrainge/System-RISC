//! Native ordinary Rust threads, not module workers or host-linked services.
use std::{cell::Cell,
          error::Error,
          sync::{Arc, Barrier, Condvar, Mutex, OnceLock, RwLock,
                 atomic::{AtomicUsize, Ordering}},
          thread,
          time::{Duration, Instant}};

use mac_toolbox::System;

static TLS_DROPS: AtomicUsize = AtomicUsize::new(0);
static INITIALIZATIONS: AtomicUsize = AtomicUsize::new(0);

struct TlsWitness(Cell<usize>);
impl Drop for TlsWitness
{
    fn drop(&mut self)
    {
        assert_ne!(self.0.get(), 0);
        TLS_DROPS.fetch_add(1, Ordering::SeqCst);
    }
}
thread_local! {
    static LOCAL: TlsWitness = const { TlsWitness(Cell::new(0)) };
}

fn child_identity(value: usize) -> (u64, usize)
{
    LOCAL.with(|local| {
             assert_eq!(local.0.replace(value), 0, "child inherited another thread's TLS");
         });
    let system = System::new().unwrap();
    let first = system.client().current_native_thread().unwrap();
    let stack = 17usize;
    let address = std::hint::black_box(&stack as *const usize as usize);
    thread::yield_now();
    assert_eq!(system.client().current_native_thread().unwrap(), first);
    LOCAL.with(|local| assert_eq!(local.0.get(), value));
    (first.0, address)
}

pub(super) fn run(system: &System) -> Result<(), Box<dyn Error>>
{
    let main = system.processes().current()?;
    let parallelism = thread::available_parallelism()?.get();
    let barrier = Arc::new(Barrier::new(4));
    let count = Arc::new(RwLock::new(0usize));
    let once = Arc::new(OnceLock::new());
    let mut handles = Vec::new();
    for index in 0..4
    {
        let barrier = barrier.clone();
        let count = count.clone();
        let once = once.clone();
        handles.push(thread::Builder::new().name(format!("std child {index}"))
                                           .spawn(move || {
                                               let identity = child_identity(index + 1);
                                               assert_eq!(System::new().unwrap()
                                                                       .processes()
                                                                       .current()
                                                                       .unwrap()
                                                                       .id,
                                                          main.id);
                                               barrier.wait();
                                               #[cfg(feature = "thread-residency")]
                                               super::residency::run(index, identity.0);
                                               LOCAL.with(|local| {
                                                        assert_eq!(local.0.get(), index + 1)
                                                    });
                                               assert_eq!(*once.get_or_init(|| {
                                    INITIALIZATIONS.fetch_add(1, Ordering::SeqCst);
                                    42usize
                                }),
                           42);
                                               for _ in 0..128
                                               {
                                                   *count.write().unwrap() += 1;
                                                   thread::yield_now();
                                               }
                                               identity
                                           })?);
    }
    let identities = handles.into_iter().map(|handle| handle.join().unwrap()).collect::<Vec<_>>();
    for (index, (id, stack)) in identities.iter().enumerate()
    {
        assert_ne!(*id, main.thread.0);
        for (other, other_stack) in &identities[..index]
        {
            assert_ne!(id, other);
            assert_ne!(stack, other_stack);
        }
    }
    assert_eq!(*count.read().unwrap(), 512);
    assert_eq!(INITIALIZATIONS.load(Ordering::SeqCst), 1);
    assert_eq!(TLS_DROPS.load(Ordering::SeqCst), 4, "JOIN preceded child TLS completion");

    // Genuine contended condition wait, followed by wake-all. The predicate and
    // waiter arrival use the same mutex; no host acknowledgment orders workers.
    let condition = Arc::new((Mutex::new((0usize, false)), Condvar::new()));
    let mut waiters = Vec::new();
    for _ in 0..2
    {
        let condition = condition.clone();
        waiters.push(thread::spawn(move || {
                         let (lock, changed) = &*condition;
                         let mut state = lock.lock().unwrap();
                         state.0 += 1;
                         changed.notify_all();
                         while !state.1
                         {
                             state = changed.wait(state).unwrap();
                         }
                         77usize
                     }));
    }
    let (lock, changed) = &*condition;
    let (mut state, timeout) = changed.wait_timeout_while(lock.lock().unwrap(),
                                                          Duration::from_secs(5),
                                                          |state| state.0 != 2)
                                      .unwrap();
    assert!(!timeout.timed_out(), "condition waiters did not arrive");
    state.1 = true;
    changed.notify_all();
    drop(state);
    for waiter in waiters
    {
        assert_eq!(waiter.join().unwrap(), 77);
    }

    // A token sent before park must be retained; a subsequent park times out.
    thread::current().unpark();
    thread::park();
    let before = Instant::now();
    thread::park_timeout(Duration::from_millis(10));
    // park_timeout may wake spuriously. Sleep has the stronger lower bound.
    thread::sleep(Duration::from_millis(10));
    assert!(before.elapsed() >= Duration::from_millis(10));
    thread::sleep(Duration::ZERO);

    // Scoped children borrow the parent's real stack. Upstream scope completion
    // must end those borrows before returning to this owner.
    let mut borrowed = [0usize; 2];
    thread::scope(|scope| {
        for (index, value) in borrowed.iter_mut().enumerate()
        {
            scope.spawn(move || {
                     *value = index + 8;
                 });
        }
    });
    assert_eq!(borrowed, [8, 9]);

    // The transfer scenario additionally exercises Rust's public Send handle
    // contract. A creator-only capture does not qualify that separate property.
    #[cfg(feature = "thread-transfer")]
    {
        let transferred = thread::spawn(|| {
            thread::sleep(Duration::from_millis(10));
            123usize
        });
        let consumer = thread::spawn(move || transferred.join().unwrap());
        assert_eq!(consumer.join().unwrap(), 123);
    }

    // Moving and dropping a handle detaches, without cancelling its child.
    let complete = Arc::new((Mutex::new(false), Condvar::new()));
    let child_complete = complete.clone();
    let detached = thread::spawn(move || {
        thread::sleep(Duration::from_millis(10));
        let (done, changed) = &*child_complete;
        *done.lock().unwrap() = true;
        changed.notify_one();
    });
    #[cfg(feature = "thread-transfer")]
    thread::spawn(move || drop(detached)).join().unwrap();
    #[cfg(not(feature = "thread-transfer"))]
    drop(detached);
    let (done, changed) = &*complete;
    let (done_guard, timeout) =
        changed.wait_timeout_while(done.lock().unwrap(), Duration::from_secs(5), |done| !*done)
               .unwrap();
    assert!(!timeout.timed_out(), "detaching cancelled the child");
    drop(done_guard);

    // OS label restrictions must not abort a valid Rust child or change its name.
    let name = "long diagnostic αβ\n".repeat(16);
    let expected_name = name.clone();
    thread::Builder::new().name(name)
                          .spawn(move || {
                              assert_eq!(thread::current().name(), Some(expected_name.as_str()));
                          })?
                          .join()
                          .unwrap();

    let budget_children = budget_refusal(system)?;
    assert_eq!(thread::spawn(|| 99usize).join().unwrap(), 99, "child reuse failed");
    let transfer = cfg!(feature = "thread-transfer");
    system.log(&format!("STD_PROBE_THREADS children={identities:?} parallelism={parallelism} \
                         tls_drops=4 rwlock=512 once=1 condition=true park=true sleep=true \
                         scope=true transfer={transfer} detach=true \
                         refusal_children={budget_children} reuse=true\n"))?;
    Ok(())
}

struct CaptureDrop(Arc<AtomicUsize>);
impl Drop for CaptureDrop
{
    fn drop(&mut self)
    {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn budget_refusal(system: &System) -> Result<usize, Box<dyn Error>>
{
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let drops = Arc::new(AtomicUsize::new(0));
    let mut children = Vec::new();
    let mut refused = false;
    for _ in 0..64
    {
        let release = release.clone();
        let witness = CaptureDrop(drops.clone());
        match thread::Builder::new().stack_size(4 * 1024 * 1024).spawn(move || {
                                                                    let (lock, changed) = &*release;
                                                                    let mut allowed =
                                                                        lock.lock().unwrap();
                                                                    while !*allowed
                                                                    {
                                                                        allowed =
                                                                            changed.wait(allowed)
                                                                                   .unwrap();
                                                                    }
                                                                    drop(allowed);
                                                                    drop(witness);
                                                                })
        {
            Ok(child) => children.push(child),
            Err(cause) =>
            {
                assert_eq!(cause.kind(), std::io::ErrorKind::OutOfMemory);
                let diagnostic = cause.to_string();
                assert!(diagnostic.len() > 20, "CREATE lost its native refusal detail");
                assert_eq!(drops.load(Ordering::SeqCst),
                           1,
                           "refused CREATE did not release its captured data exactly once");
                system.log(&format!("STD_PROBE_THREAD_REFUSAL {diagnostic}\n"))?;
                refused = true;
                break;
            }
        }
    }
    assert!(refused, "configured process stack budget did not refuse bounded creation");
    assert!(!children.is_empty());
    let count = children.len();
    let (lock, changed) = &*release;
    *lock.lock().unwrap() = true;
    changed.notify_all();
    for child in children
    {
        child.join().unwrap();
    }
    assert_eq!(drops.load(Ordering::SeqCst), count + 1);
    Ok(count)
}
