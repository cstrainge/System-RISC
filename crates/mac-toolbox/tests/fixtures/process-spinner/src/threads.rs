//! Native same-process child workload. Uses the real CRT transport, application
//! heap, TLS and scheduler; no host-linked service or per-operation host barrier.
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{fmt::Write,
           sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering},
           time::Duration};

use mac_toolbox::{Client, Error, ProcessId, System, ThreadId, ToolboxResult,
                  abi::{native_threads as thread, tls},
                  native_threads};

const LIMIT: usize = 32;
const ITERATIONS: u32 = 256;
static INITIALIZED_GATE: AtomicU32 = AtomicU32::new(7);
static ZEROED_GATE: AtomicU32 = AtomicU32::new(0);

struct Shared
{
    app:            ProcessId,
    key:            u64,
    started:        AtomicU32,
    gate:           AtomicU32,
    destructors:    AtomicU32,
    inputs_dropped: AtomicU32,
    sum:            AtomicU32,
    threads:        [AtomicU64; LIMIT],
    stacks:         [AtomicUsize; LIMIT]
}
struct Input
{
    shared:       Arc<Shared>,
    slot:         usize,
    parent_stack: *const AtomicU32
}
impl Drop for Input
{
    fn drop(&mut self)
    {
        self.shared.inputs_dropped.fetch_add(1, Ordering::AcqRel);
    }
}
struct TlsValue
{
    shared: Arc<Shared>,
    thread: ThreadId
}

fn call(client: Client<'_>, mut request: thread::Call)
        -> core::result::Result<thread::Call, native_threads::RequestFailure>
{
    // SAFETY: CREATE below transfers one real Input only on OK. Entries belong
    // to this initialized application; the entry drains its actual TLS before
    // returning. All joins/queries use rights this Process actually received.
    unsafe {
        client.native_thread(&mut request)?;
    }
    Ok(request)
}

fn tls_call(client: Client<'_>, mut request: tls::Call) -> native_threads::Result<tls::Call>
{
    // SAFETY: callback is this image's C function; every nonzero callback value
    // is a live unique TlsValue box until that exact thread's destructor takes it.
    unsafe {
        client.tls(&mut request)?;
    }
    Ok(request)
}

unsafe extern "C" fn destructor(pointer: *mut u8)
{
    // SAFETY: this thread's SET transferred exactly one live TlsValue box.
    let value = unsafe { Box::from_raw(pointer.cast::<TlsValue>()) };
    let system = System::new().expect("child destructor has the installed native runtime");
    assert_eq!(system.client().current_native_thread().expect("actual destructor THRD"),
               value.thread);
    value.shared.destructors.fetch_add(1, Ordering::AcqRel);
    system.client().wake_all_u32(&value.shared.destructors).expect("TLS completion wake");
}

fn await_value(client: Client<'_>, atomic: &AtomicU32, value: u32)
{
    let deadline = client.monotonic_nanoseconds().expect("native clock") + 10_000_000_000;
    loop
    {
        let observed = atomic.load(Ordering::Acquire);
        if observed >= value
        {
            return;
        }
        assert!(client.monotonic_nanoseconds().expect("native clock") < deadline,
                "native child rendezvous timed out");
        client.wait_u32(atomic, observed, Some(Duration::from_millis(50)))
              .expect("real atomic wait while child is active");
    }
}

unsafe extern "C" fn entry(data: *mut u8)
{
    // SAFETY: the last successful CREATE commit transfers this one Input box;
    // refused CREATE never executes this function and leaves it with its caller.
    let input = unsafe { Box::from_raw(data.cast::<Input>()) };
    let shared = input.shared.clone();
    let system = System::new().expect("native child sees the one application service installation");
    let client = system.client();
    let current = client.current_native_thread().expect("genuine child THRD");
    assert_eq!(system.processes().current().expect("child Process").id, shared.app);
    shared.threads[input.slot].store(current.0, Ordering::Release);
    let own_stack = AtomicU32::new(3);
    shared.stacks[input.slot].store(core::ptr::addr_of!(own_stack).addr(), Ordering::Release);
    let name = b"Native wait child";
    call(client,
         thread::Call { operation: thread::NAME,
                        name: name.as_ptr(),
                        name_length: name.len() as u32,
                        ..Default::default() }).expect("child can name its actual row");
    let value = Box::into_raw(Box::new(TlsValue { shared: shared.clone(),
                                                  thread: current }));
    if let Err(cause) = tls_call(client,
                                 tls::Call { operation: tls::SET,
                                             key: shared.key,
                                             value: value.addr(),
                                             ..Default::default() })
    {
        if matches!(cause, native_threads::Failure::Resident { .. })
        {
            // SAFETY: an actual resident refusal did not publish this value.
            unsafe {
                drop(Box::from_raw(value));
            }
        }
        // Protocol damage could follow a successful SET: leave that value
        // alive and take the Process abort path rather than guessing ownership.
        panic!("child TLS admission failed: {cause}");
    }
    assert_eq!(tls_call(client,
                        tls::Call { operation: tls::GET,
                                    key: shared.key,
                                    ..Default::default() }).expect("child TLS read")
                                                           .value,
               value.addr());
    shared.started.fetch_add(1, Ordering::Release);
    client.wake_all_u32(&shared.started).expect("notify actual started count");
    await_value(client, &shared.gate, 1);
    // SAFETY: the parent owns this AtomicU32 until all successful children are
    // joined. It is Sync; every access is atomic. The System pins its real stack.
    let parent_stack = unsafe { &*input.parent_stack };
    await_value(client, parent_stack, 1);
    await_value(client, &INITIALIZED_GATE, 8);
    await_value(client, &ZEROED_GATE, 1);
    assert_eq!(client.wait_u32(&own_stack, 3, Some(Duration::from_millis(2)))
                     .expect("independent child stack wait"),
               mac_toolbox::address_wait::WaitOutcome::TimedOut);
    for iteration in 0..ITERATIONS
    {
        let mut bytes = Box::new([0u8; 128]);
        bytes[0] = input.slot as u8;
        bytes[127] = iteration as u8;
        core::hint::black_box(&mut bytes);
        assert_eq!(bytes[0], input.slot as u8);
        assert_eq!(bytes[127], iteration as u8);
        shared.sum.fetch_add(1, Ordering::Relaxed);
        if iteration % 31 == 0
        {
            call(client,
                 thread::Call { operation: thread::YIELD,
                                ..Default::default() }).expect("actual child yields without \
                                                                losing its continuation");
        }
    }
    drop(input);
    // SAFETY: this actual child runs every callback and acknowledges each
    // receipt exactly once. Resident return may proceed only after FINISH.
    unsafe {
        native_threads::teardown_tls(client).expect("child CRT TLS teardown");
    }
}

pub fn run(system: &System<'static>) -> ToolboxResult<()>
{
    let client = system.client();
    let process = system.processes().current()?;
    let main = client.current_native_thread().map_err(|e| e.cause())?;
    let parallelism = call(client,
                           thread::Call { operation: thread::PARALLELISM,
                                          ..Default::default() }).map_err(|e| e.cause())?
                                                                 .parallelism;
    assert!(parallelism >= 1);
    let key = tls_call(client,
                       tls::Call { destructor: destructor as *const () as usize,
                                   ..Default::default() }).map_err(|e| e.cause())?
                                                          .key;
    let shared = Arc::new(Shared { app: process.id,
                                   key,
                                   started: AtomicU32::new(0),
                                   gate: AtomicU32::new(0),
                                   destructors: AtomicU32::new(0),
                                   inputs_dropped: AtomicU32::new(0),
                                   sum: AtomicU32::new(0),
                                   threads: core::array::from_fn(|_| AtomicU64::new(0)),
                                   stacks: core::array::from_fn(|_| AtomicUsize::new(0)) });
    let parent_stack = AtomicU32::new(0);
    let mut rights = Vec::with_capacity(LIMIT);
    let mut refused = false;
    for slot in 0..LIMIT
    {
        let input = Box::into_raw(Box::new(Input { shared: shared.clone(),
                                                   slot,
                                                   parent_stack: &parent_stack }));
        let result = call(client,
                          thread::Call { operation: thread::CREATE,
                                         entry: entry as *const () as usize,
                                         data: input.addr(),
                                         stack_bytes: 0,
                                         ..Default::default() });
        match result
        {
            Ok(created) =>
            {
                assert_ne!(ThreadId(created.thread), main);
                assert_ne!(created.right, 0);
                rights.push((ThreadId(created.thread), created.right));
            }
            Err(cause) =>
            {
                if cause.create_transfer() != Some(native_threads::CreateTransfer::Retained)
                {
                    // A malformed reply does not establish who owns Input.
                    // Keep its address live and abort this Process; never free
                    // storage which an already accepted child may be using.
                    panic!("CREATE transfer is indeterminate: {cause}");
                }
                // SAFETY: the typed resident refusal establishes no transfer.
                unsafe {
                    drop(Box::from_raw(input));
                }
                assert_eq!(cause.cause(), Error::NoMemory, "child budget refusal is typed");
                refused = true;
                break;
            }
        }
    }
    assert!(refused && rights.len() >= 2, "configured Process budget bounds actual child stacks");
    await_value(client, &shared.started, rights.len() as u32);
    let main_value = tls_call(client,
                              tls::Call { operation: tls::GET,
                                          key,
                                          ..Default::default() }).map_err(|e| e.cause())?
                                                                 .value;
    assert_eq!(main_value, 0, "children cannot change parent's independent TLS slots");
    // Release before/while peers enter each different backing's wait. There is
    // no host acknowledgement between operations and no assumed wake count.
    shared.gate.store(1, Ordering::Release);
    client.wake_all_u32(&shared.gate).map_err(|e| e.cause())?;
    parent_stack.store(1, Ordering::Release);
    client.wake_all_u32(&parent_stack).map_err(|e| e.cause())?;
    INITIALIZED_GATE.store(8, Ordering::Release);
    client.wake_all_u32(&INITIALIZED_GATE).map_err(|e| e.cause())?;
    ZEROED_GATE.store(1, Ordering::Release);
    client.wake_all_u32(&ZEROED_GATE).map_err(|e| e.cause())?;
    let mut joined = 0u32;
    for &(child, right) in rights.iter().rev()
    {
        let result = call(client,
                          thread::Call { operation: thread::JOIN,
                                         right,
                                         ..Default::default() }).map_err(|e| e.cause())?;
        assert_eq!(ThreadId(result.thread), child);
        assert_eq!(result.outcome, thread::NORMAL_OUTCOME);
        joined += 1;
        assert!(shared.destructors.load(Ordering::Acquire) >= joined,
                "join cannot precede actual child TLS destruction");
        let stale = call(client,
                         thread::Call { operation: thread::QUERY,
                                        right,
                                        ..Default::default() });
        assert!(matches!(stale, Err(e) if e.cause() == Error::NotFound),
                "consumed join right cannot alias a subsequent child");
    }
    assert_eq!(shared.sum.load(Ordering::Acquire), rights.len() as u32 * ITERATIONS);
    assert_eq!(shared.destructors.load(Ordering::Acquire), rights.len() as u32);
    assert_eq!(shared.inputs_dropped.load(Ordering::Acquire), rights.len() as u32 + 1);
    for (slot, &(child, _)) in rights.iter().enumerate()
    {
        assert_eq!(ThreadId(shared.threads[slot].load(Ordering::Acquire)), child);
        let address = shared.stacks[slot].load(Ordering::Acquire);
        assert_ne!(address, 0);
        for other in &shared.stacks[..slot]
        {
            assert_ne!(address, other.load(Ordering::Acquire));
        }
        let mut text = crate::LogBuffer::new();
        writeln!(text,
                 "NATIVE_CHILD_JOINED app={} thread={child} stack={address:#x}",
                 process.id.0).map_err(|_| Error::BadAbi)?;
        system.log(text.text())?;
    }
    tls_call(client,
             tls::Call { operation: tls::DELETE,
                         key,
                         ..Default::default() }).map_err(|e| e.cause())?;
    protocol_cases(system);
    let mut text = crate::LogBuffer::new();
    writeln!(text,
             "NATIVE_THREADS_BODY main={main} children={} parallelism={parallelism} \
              budget_refusal=true tls=true waits=true allocations=true joined=true",
             rights.len()).map_err(|_| Error::BadAbi)?;
    system.log(text.text())?;
    Ok(())
}

struct ProtocolProbe
{
    gate:      AtomicU32,
    right:     AtomicU64,
    done:      AtomicU32,
    self_join: bool
}
fn protocol_probe(self_join: bool) -> Arc<ProtocolProbe>
{
    Arc::new(ProtocolProbe { gate: AtomicU32::new(0),
                             right: AtomicU64::new(0),
                             done: AtomicU32::new(0),
                             self_join })
}
fn refused(client: Client<'_>, request: thread::Call, expected: Error)
{
    let failure = match call(client, request)
    {
        Ok(_) => panic!("malformed or forbidden operation must refuse"),
        Err(failure) => failure
    };
    assert_eq!(failure.cause(), expected);
    match failure.diagnostic()
    {
        native_threads::Failure::Resident { detail,
                                            .. } =>
        {
            assert!(!detail.as_str().is_empty(), "resident refusal must retain useful detail")
        }
        native_threads::Failure::Protocol(detail) => panic!("damaged refusal: {detail}")
    }
}
unsafe extern "C" fn protocol_entry(data: *mut u8)
{
    // SAFETY: accepted CREATE transfers exactly one strong Arc reference.
    let probe = unsafe { Arc::from_raw(data.cast::<ProtocolProbe>()) };
    let system = System::new().expect("native protocol child runtime");
    let client = system.client();
    await_value(client, &probe.gate, 1);
    if probe.self_join
    {
        refused(client,
                thread::Call { operation: thread::JOIN,
                               right: probe.right.load(Ordering::Acquire),
                               ..Default::default() },
                Error::BadArgument);
    }
    call(client,
         thread::Call { operation: thread::SLEEP,
                        nanoseconds: 1_000_000,
                        ..Default::default() }).expect("real child sleep resumes");
    // SAFETY: actual child drains its own callbacks before the return continuation.
    unsafe { native_threads::teardown_tls(client) }.expect("protocol child TLS finish");
    probe.done.store(1, Ordering::Release);
    client.wake_all_u32(&probe.done).expect("protocol child completion signal");
}
fn raw_probe(client: Client<'_>, probe: &Arc<ProtocolProbe>) -> thread::Call
{
    let data = Arc::into_raw(probe.clone());
    match call(client,
               thread::Call { operation: thread::CREATE,
                              entry: protocol_entry as *const () as usize,
                              data: data.addr(),
                              ..Default::default() })
    {
        Ok(created) => created,
        Err(cause) =>
        {
            if cause.create_transfer() == Some(native_threads::CreateTransfer::Retained)
            {
                // SAFETY: actual resident refusal retained this exact Arc reference.
                unsafe {
                    drop(Arc::from_raw(data));
                }
            }
            panic!("post-reclamation child creation failed: {cause}");
        }
    }
}
fn protocol_cases(system: &System<'static>)
{
    let client = system.client();
    for request in [thread::Call { flags: 1,
                                   ..Default::default() },
                    thread::Call { operation: thread::EXIT,
                                   ..Default::default() },
                    thread::Call { operation: thread::SLEEP,
                                   nanoseconds: 1_000_000_000,
                                   ..Default::default() },
                    thread::Call { operation: thread::SLEEP,
                                   seconds: u64::MAX,
                                   ..Default::default() },
                    thread::Call { operation: thread::CREATE,
                                   entry: protocol_entry as *const () as usize,
                                   stack_bytes: 1,
                                   ..Default::default() }]
    {
        refused(client, request, Error::BadArgument);
    }
    let malformed = [0xff];
    refused(client,
            thread::Call { operation: thread::NAME,
                           name: malformed.as_ptr(),
                           name_length: 1,
                           ..Default::default() },
            Error::BadArgument);
    let probe = protocol_probe(true);
    let created = raw_probe(client, &probe);
    probe.right.store(created.right, Ordering::Release);
    probe.gate.store(1, Ordering::Release);
    client.wake_all_u32(&probe.gate).expect("self-join child gate");
    call(client,
         thread::Call { operation: thread::JOIN,
                        right: created.right,
                        ..Default::default() }).expect("self-join refusal leaves legitimate \
                                                        parent join usable");
    assert_eq!(probe.done.load(Ordering::Acquire), 1);

    let detached = protocol_probe(false);
    let created = raw_probe(client, &detached);
    call(client,
         thread::Call { operation: thread::DETACH,
                        right: created.right,
                        ..Default::default() }).expect("detach before completion");
    refused(client,
            thread::Call { operation: thread::QUERY,
                           right: created.right,
                           ..Default::default() },
            Error::NotFound);
    refused(client,
            thread::Call { operation: thread::DETACH,
                           right: created.right,
                           ..Default::default() },
            Error::NotFound);
    detached.gate.store(1, Ordering::Release);
    client.wake_all_u32(&detached.gate).expect("detached child remains live");
    await_value(client, &detached.done, 1);

    let raii = protocol_probe(false);
    let data = Arc::into_raw(raii.clone());
    // SAFETY: entry/data satisfy the C contract. The affine owner remains on
    // this creating main thread, in ordinary preemptible application context.
    let right =
        match unsafe { client.create_native_thread(protocol_entry, data.cast_mut().cast(), 0) }
        {
            Ok(right) => right,
            Err(cause) =>
            {
                if cause.transfer() == native_threads::CreateTransfer::Retained
                {
                    // SAFETY: no child owns this rejected strong reference.
                    unsafe {
                        drop(Arc::from_raw(data));
                    }
                }
                panic!("RAII child creation failed: {cause}");
            }
        };
    let raii_thread = right.thread();
    drop(right);
    raii.gate.store(1, Ordering::Release);
    client.wake_all_u32(&raii.gate).expect("RAII detach preserves child execution");
    await_value(client, &raii.done, 1);
    let mut text = crate::LogBuffer::new();
    writeln!(text,
             "NATIVE_THREAD_PROTOCOL self_join=true refusals=true detach=true raii=true \
              reuse=true detached={} raii_thread={}",
             created.thread, raii_thread).expect("bounded protocol diagnostic");
    system.log(text.text()).expect("native protocol summary");
}
