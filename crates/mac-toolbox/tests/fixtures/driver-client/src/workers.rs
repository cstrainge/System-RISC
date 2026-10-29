//! Guest assertions execute through the actual public module SDK. Native
//! clock/log/negative-ABI observations are isolated in machine.rs. This is a
//! bounded lifecycle scenario, not stress/SMP or arbitrary-fault qualification.
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use mac_abi::{BAD_ARGUMENT, NO_MEMORY, NOT_FOUND, NOT_SUPPORTED, OPERATION_CANCELLED,
              SERVICE_BUSY, WRONG_THREAD};
use mac_driver::{Client, Completion, Error, Phase, Provider, Result, sync::Mutex};
use mac_driver_alloc::{PreparedWorker, Worker, WorkerAffinity, WorkerContext, Workers};

use crate::machine;
const SECOND: u64 = 10_000_000;
const LIMIT: u64 = 15 * SECOND;
static INITIALIZING: AtomicBool = AtomicBool::new(true);
static PASS_READY: AtomicBool = AtomicBool::new(false);
static ENTERED: AtomicU32 = AtomicU32::new(0);
static DROPPED: AtomicU32 = AtomicU32::new(0);
static SPARE_DROPPED: AtomicU32 = AtomicU32::new(0);
static FAILED_DROPPED: AtomicU32 = AtomicU32::new(0);
static PEER_DROPPED: AtomicU32 = AtomicU32::new(0);
static PARKED_DROPPED: AtomicU32 = AtomicU32::new(0);
static PEER_PHASE: AtomicU32 = AtomicU32::new(0);
static PEER_TOKEN: AtomicU64 = AtomicU64::new(0);
static PARKED_TOKEN: AtomicU64 = AtomicU64::new(0);
static SHARED: Mutex<u64> = Mutex::new(0);
struct Controllers
{
    coordinator: Option<Worker>,
    peer:        Option<Worker>
}
static CONTROLLERS: Mutex<Controllers> = Mutex::new(Controllers { coordinator: None,
                                                                  peer:        None });
struct CaptureDrop(&'static AtomicU32);
impl Drop for CaptureDrop
{
    fn drop(&mut self)
    {
        self.0.fetch_add(1, Ordering::AcqRel);
        let total = DROPPED.fetch_add(1, Ordering::AcqRel) + 1;
        if !cfg!(feature = "workers-rollback")
           && core::ptr::eq(self.0, &DROPPED_COORDINATOR)
           && PASS_READY.load(Ordering::Acquire)
        {
            assert_eq!(total, 5, "captured closure destruction was not balanced");
            machine::log(format_args!("MODULE_WORKERS_PASS actors=3 armed=2 \
                                       io=start-poll-cancel wake=coalesced \
                                       cancel=running-parked tokens=stale-foreign captures=5 \
                                       budget=refused mutex=bounded\n"));
        }
        if cfg!(feature = "workers-rollback") && total == 4
        {
            assert_eq!(ENTERED.load(Ordering::Acquire),
                       0,
                       "an armed worker entered before failed commit");
            machine::log(format_args!("MODULE_WORKER_ROLLBACK_CAPTURES_PASS entered=0 \
                                       dropped=4\n"));
        }
    }
}
fn until(label: &str, mut done: impl FnMut() -> bool)
{
    let start = machine::now();
    while !done()
    {
        assert!(machine::now().wrapping_sub(start) < LIMIT,
                "worker native progress timed out: {label}");
        core::hint::spin_loop();
    }
}
fn prepare<F>(host: Client, name: &str, mut body: F) -> PreparedWorker
    where F: FnOnce(WorkerContext) -> Result<()> + Send + 'static
{
    let start = machine::now();
    loop
    {
        match host.prepare_worker(name, body)
        {
            Ok(worker) => return worker,
            Err(failure) =>
            {
                assert_eq!(failure.cause, Error(NO_MEMORY), "unexpected worker prepare refusal");
                assert!(machine::now().wrapping_sub(start) < LIMIT,
                        "exited worker backing did not become reusable");
                body = failure.body;
            }
        }
    }
}
fn start(worker: PreparedWorker) -> Worker
{
    worker.start().unwrap_or_else(|failure| panic!("worker start refused {}", failure.cause.0))
}
fn entered()
{
    assert!(!INITIALIZING.load(Ordering::Acquire), "armed worker entered during initialization");
    ENTERED.fetch_add(1, Ordering::AcqRel);
}
pub fn initialize(host: Client, provider: Provider) -> Result<()>
{
    machine::worker_descriptor_refusals();
    let capture = CaptureDrop(&PEER_DROPPED);
    let peer =
        host.prepare_worker_with_affinity("wake/cancel peer",
                                          WorkerAffinity::Primary,
                                          move |context| {
                                              let _capture = capture;
                                              entered();
                                              peer(context, provider)
                                          })
            .unwrap_or_else(|failure| panic!("initial peer prepare refused {}", failure.cause.0));
    let capture = CaptureDrop(&DROPPED_COORDINATOR);
    let coordinator =
        host.prepare_worker("worker coordinator", move |context| {
                let _capture = capture;
                entered();
                coordinator(context, host, provider)
            })
            .unwrap_or_else(|failure| panic!("coordinator prepare refused {}", failure.cause.0));
    let capture = CaptureDrop(&SPARE_DROPPED);
    let spare =
        host.prepare_worker("inert disposal", move |_| {
                let _capture = capture;
                panic!("disposed inert worker unexpectedly entered")
            })
            .unwrap_or_else(|failure| {
                panic!("third 4 MiB preparation refused {}", failure.cause.0)
            });
    let capture = CaptureDrop(&FAILED_DROPPED);
    match host.prepare_worker("budget refusal", move |_| {
                  let _capture = capture;
                  Ok(())
              })
    {
        Ok(_) => panic!("fourth 4 MiB worker exceeded genuine 16 MiB module budget"),
        Err(failure) =>
        {
            assert_eq!(failure.cause, Error(NO_MEMORY), "worker budget refusal lost its cause");
            assert_eq!(FAILED_DROPPED.load(Ordering::Acquire),
                       0,
                       "failed preparation destroyed the caller's body");
            drop(failure.body);
            assert_eq!(FAILED_DROPPED.load(Ordering::Acquire),
                       1,
                       "returned body was not owned by caller");
        }
    }
    drop(spare); // DISPOSE records intent; the real reaper destroys it later.
    let peer = start(peer);
    let coordinator = start(coordinator);
    CONTROLLERS.with(|state| {
                   state.peer = Some(peer);
                   state.coordinator = Some(coordinator);
               })?;
    // Span real timer opportunities while both starts are merely armed. No
    // host acknowledgment or fixture scheduler barrier makes them dormant.
    let began = machine::now();
    while machine::now().wrapping_sub(began) < SECOND / 20
    {
        assert_eq!(ENTERED.load(Ordering::Acquire), 0, "precommit worker ran");
        core::hint::spin_loop();
    }
    if cfg!(feature = "workers-rollback")
    {
        return Err(Error(BAD_ARGUMENT));
    }
    INITIALIZING.store(false, Ordering::Release);
    Ok(())
}
static DROPPED_COORDINATOR: AtomicU32 = AtomicU32::new(0);
fn peer(context: WorkerContext, provider: Provider) -> Result<()>
{
    PEER_TOKEN.store(machine::current_token()?, Ordering::Release);
    // Two actual runnable workers share the native mutex before this peer parks.
    for _ in 0..4096
    {
        SHARED.with(|value| *value += 1)?;
    }
    let waker = context.waker();
    waker.wake()?;
    waker.wake()?;
    waker.wake()?;
    context.park()?; // exactly one coalesced wake is consumed here.
    PEER_PHASE.store(1, Ordering::Release);
    context.park_for(core::time::Duration::from_secs(60))?;
    // The coordinator's real wake must resume this before the long deadline.
    PEER_PHASE.store(2, Ordering::Release);
    for _ in 0..2048
    {
        SHARED.with(|value| *value += 1)?;
    }
    until("running cancellation", || {
        context.cancelled().unwrap_or_else(|error| panic!("cancel query {}", error.0))
    });
    assert_eq!(context.park(), Err(Error(OPERATION_CANCELLED)), "cancelled running worker parked");
    assert_eq!(context.park(), Err(Error(OPERATION_CANCELLED)), "cancellation was not sticky");
    assert_eq!(context.park_for(core::time::Duration::from_millis(1)),
               Err(Error(OPERATION_CANCELLED)));
    let _ = provider; // dependency is exercised by the other actual worker.
    Ok(())
}
fn coordinator(context: WorkerContext, host: Client, provider: Provider) -> Result<()>
{
    machine::log(format_args!("MODULE_WORKER_ACTIVE actors=2\n"));
    // Real private worker traps, actual canonical wait row and timer selection;
    // no host clock, replacement scheduler or per-operation host ACK.
    let before = context.monotonic_microseconds()?;
    context.park_for(core::time::Duration::ZERO)?;
    assert!(context.monotonic_microseconds()? >= before);
    assert_eq!(context.park_for(core::time::Duration::MAX), Err(Error(mac_abi::BAD_ARGUMENT)));
    assert_eq!(context.park_for(core::time::Duration::from_micros(u64::MAX)),
               Err(Error(mac_abi::BAD_ARGUMENT)));
    let wake = context.waker();
    wake.wake()?;
    wake.wake()?;
    wake.wake()?;
    context.park_for(core::time::Duration::from_secs(60))?;
    let before = context.monotonic_microseconds()?;
    context.park_for(core::time::Duration::from_millis(30))?;
    let elapsed = context.monotonic_microseconds()?
                         .checked_sub(before)
                         .expect("native monotonic worker clock invariant");
    assert!(elapsed >= 30_000, "coalesced wake leaked into the next timed park: {elapsed}");
    machine::log(format_args!("MODULE_WORKER_TIMER_PASS elapsed_microseconds={} zero=ok \
                               conversion_overflow=refused deadline_overflow=refused \
                               wake=coalesced\n",
                              elapsed));
    let active = machine::now();
    // Real CPU work remains preemptible while Finder creates and services the
    // desktop. The host injects one observational input, never per-operation ACKs.
    let mut turns = 0u64;
    while machine::now().wrapping_sub(active) < 5 * SECOND
    {
        if turns & 255 == 0
        {
            SHARED.with(|value| *value += 1)?;
        }
        turns = turns.wrapping_add(1);
        core::hint::spin_loop();
    }
    assert!(turns != 0);
    SHARED.with(|value| {
              assert_eq!(SHARED.with(|_| -> () { panic!("contended callback ran") }),
                         Err(Error(SERVICE_BUSY)));
              assert_eq!(SHARED.try_with(|_| -> () { panic!("contended try callback ran") }),
                         Ok(None));
              assert_eq!(context.park(),
                         Err(Error(WRONG_THREAD)),
                         "park inside critical section was accepted");
              assert_eq!(context.park_for(core::time::Duration::from_millis(1)),
                         Err(Error(WRONG_THREAD)));
              assert_eq!(context.monotonic_microseconds(), Err(Error(WRONG_THREAD)));
              let mut ticket = 0;
              let mut output = [0; 32];
              assert_eq!(provider.invoke(&mut ticket, Phase::Start, 7, b"blocked", &mut output),
                         Err(Error(WRONG_THREAD)),
                         "foreign module entered while mutex held");
              assert_eq!(ticket, 0, "refused critical broker call published an operation");
              *value += 1;
          })?;
    let mut output = [0; 32];
    let token = machine::current_token()?;
    let mut foreign = 0;
    assert_eq!(provider.invoke(&mut foreign, Phase::Start, 11, &token.to_le_bytes(), &mut output)?,
               Completion::Complete(4));
    assert_eq!(i32::from_le_bytes(output[..4].try_into().unwrap()), mac_abi::ACCESS_DENIED);
    assert_eq!(foreign, 0);
    let mut ticket = 0;
    assert_eq!(provider.invoke(&mut ticket, Phase::Start, 8, &[], &mut output)?,
               Completion::Pending);
    assert_ne!(ticket, 0);
    let root = ticket;
    for _ in 0..3
    {
        assert_eq!(provider.invoke(&mut ticket, Phase::Poll, 8, &[], &mut output)?,
                   Completion::Pending);
        assert_eq!(ticket, root, "worker lost its durable broker root");
    }
    let wait = machine::now();
    while machine::now().wrapping_sub(wait) < SECOND / 50
    {
        core::hint::spin_loop();
    }
    let mut cancels = 0;
    loop
    {
        cancels += 1;
        match provider.invoke(&mut ticket, Phase::Cancel, 8, &[], &mut output)
        {
            Ok(Completion::Pending) =>
            {
                assert_eq!(ticket, root);
                assert!(cancels <= 4);
            }
            Err(Error(NOT_SUPPORTED)) =>
            {
                assert_eq!(ticket, 0, "terminal provider ACK retained worker DIOP");
                break;
            }
            other => panic!("unexpected worker broker cancellation {other:?}")
        }
    }
    assert_eq!(cancels, 3);
    until("second park", || PEER_PHASE.load(Ordering::Acquire) == 1);
    let peer_token = PEER_TOKEN.load(Ordering::Acquire);
    until("actual coalesced park", || {
        machine::state(peer_token).unwrap() == mac_driver::abi::worker::PARKED
    });
    assert_eq!(PEER_PHASE.load(Ordering::Acquire),
               1,
               "wake requests counted instead of coalescing");
    let stale = CONTROLLERS.with(|state| state.peer.as_ref().unwrap().waker())?;
    stale.wake()?;
    until("peer running", || PEER_PHASE.load(Ordering::Acquire) == 2);
    CONTROLLERS.with(|state| state.peer.as_ref().unwrap().cancel())??;
    until("peer result", || {
        CONTROLLERS.with(|state| state.peer.as_ref().unwrap().try_join())
                   .unwrap()
                   .unwrap()
                   .is_some()
    });
    assert_eq!(CONTROLLERS.with(|state| state.peer.as_ref().unwrap().try_join())??, Some(Ok(())));
    CONTROLLERS.with(|state| drop(state.peer.take()))?;
    until("stale waker", || {
        match stale.wake()
        {
            Err(Error(NOT_FOUND)) => true,
            Ok(()) => false,
            Err(error) => panic!("stale token refusal {}", error.0)
        }
    });
    assert_eq!(PEER_DROPPED.load(Ordering::Acquire), 1);
    until("inert capture destruction", || SPARE_DROPPED.load(Ordering::Acquire) == 1);
    let capture = CaptureDrop(&PARKED_DROPPED);
    let parked = start(prepare(host, "parked cancellation", move |context| {
                           let _capture = capture;
                           entered();
                           PARKED_TOKEN.store(machine::current_token()?, Ordering::Release);
                           assert_eq!(context.park_for(core::time::Duration::from_secs(60)),
                                      Err(Error(OPERATION_CANCELLED)),
                                      "parked cancellation did not resume with sticky refusal");
                           assert!(context.cancelled()?);
                           assert_eq!(context.park(), Err(Error(OPERATION_CANCELLED)));
                           Ok(())
                       }));
    until("parked cancellation state", || {
        let token = PARKED_TOKEN.load(Ordering::Acquire);
        token != 0 && machine::state(token).unwrap() == mac_driver::abi::worker::PARKED
    });
    parked.cancel()?;
    until("parked result", || parked.try_join().unwrap().is_some());
    assert_eq!(parked.try_join()?, Some(Ok(())));
    let stale_parked = parked.waker();
    drop(parked);
    until("parked token retirement", || {
        match stale_parked.wake()
        {
            Err(Error(NOT_FOUND)) => true,
            Ok(()) => false,
            Err(error) => panic!("parked stale token {}", error.0)
        }
    });
    assert_eq!(PARKED_DROPPED.load(Ordering::Acquire), 1);
    assert_eq!(ENTERED.load(Ordering::Acquire), 3);
    assert_eq!(DROPPED.load(Ordering::Acquire), 4); // peer,inert,failed,parked; coordinator still runs.
    CONTROLLERS.with(|state| drop(state.coordinator.take()))?;
    PASS_READY.store(true, Ordering::Release);
    Ok(())
}
