//! Actual native APPL TLS workload through public transport and real CRT
//! teardown. This is not a host-linked resident fixture or a Rust test.
use alloc::boxed::Box;
use core::{fmt::Write,
           sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering}};

use mac_toolbox::{Client, Error, System, ToolboxResult, abi::tls as wire};
static CLIENT: AtomicPtr<Client<'static>> = AtomicPtr::new(core::ptr::null_mut());
static KEY: AtomicU64 = AtomicU64::new(0);
static COUNT: AtomicUsize = AtomicUsize::new(0);
fn invalid_callback_requests()
{
    let stack_word = 0u32;
    for address in [1usize,
                    usize::MAX - 1,
                    core::ptr::addr_of!(KEY).addr(),
                    core::ptr::addr_of!(stack_word).addr()]
    {
        let mut call = wire::Call { destructor: address,
                                    ..Default::default() };
        let mut result = core::ptr::from_mut(&mut call).addr();
        // SAFETY: deliberately malformed numeric callback metadata reaches the
        // real native trap. The request is live/exclusive; this does not invoke
        // or construct a Rust function pointer from the invalid address.
        unsafe {
            core::arch::asm!("ecall", inlateout("a0") result,
                in("a1") core::mem::size_of::<wire::Call>(),
                in("a7") mac_toolbox::abi::traps::TLS, options(nostack));
        }
        assert_eq!(result as isize,
                   mac_toolbox::abi::ACCESS_DENIED as isize,
                   "non-executable TLS callback address must be refused");
        assert_eq!(call.key, 0, "refused callback must not publish a key");
        assert!(call.detail_length > 0 && call.detail_length as usize <= call.detail.len(),
                "callback refusal must retain bounded diagnostics");
    }
}
fn exchange(client: Client<'_>, mut call: wire::Call)
            -> Result<wire::Call, mac_toolbox::native_threads::Failure>
{
    // SAFETY: fixture callbacks have the required C signature and actual image
    // lifetime. The real current application thread owns every protocol call.
    unsafe {
        client.tls(&mut call)?;
    }
    Ok(call)
}
unsafe extern "C" fn destructor(value: *mut u8)
{
    // SAFETY: run published a leaked real-heap Client with static loader-table
    // lifetime. Process exit's canonical walk owns its final storage disposal.
    let client =
        unsafe { CLIENT.load(Ordering::Acquire).as_ref() }.copied()
                                                          .expect("native fixture published \
                                                                   client before destructor");
    let count = COUNT.fetch_add(1, Ordering::AcqRel) + 1;
    assert_eq!(value.addr(), count, "TLS fixture destructor payload/round differs");
    let wrong = exchange(client,
                         wire::Call { operation: wire::ACK,
                                      ticket: u64::MAX,
                                      ..Default::default() });
    assert!(matches!(wrong,Err(cause) if cause.cause()==Error::BadArgument),
            "wrong TLS ACK must not consume original receipt");
    if count < 3
    {
        exchange(client,
                 wire::Call { operation: wire::SET,
                              key: KEY.load(Ordering::Acquire),
                              value: count + 1,
                              ..Default::default() }).expect("native fixture can reinitialize \
                                                              same TLS key for next round");
    }
    else
    {
        let key = KEY.load(Ordering::Acquire);
        exchange(client,
                 wire::Call { operation: wire::DELETE,
                              key,
                              ..Default::default() }).expect("TLS key deletion preserves \
                                                              in-flight destructor code pin");
        let stale = exchange(client,
                             wire::Call { operation: wire::GET,
                                          key,
                                          ..Default::default() });
        assert!(matches!(stale,Err(cause) if cause.cause()==Error::NotFound),
                "deleted in-flight key must be stale");
        client.log("TLS_PROBE_CALLBACKS count=3 deleted_inflight=true wrong_ack_retained=true\n")
              .expect("native fixture callback log");
    }
}
pub fn run(system: &System<'static>) -> ToolboxResult<()>
{
    crate::address_wait::run(system)?;
    invalid_callback_requests();
    let client = system.client();
    let actual = client.current_native_thread().map_err(|cause| cause.cause())?;
    assert_eq!(actual, system.processes().current()?.thread, "CURRENT is actual initial THRD");
    let key = exchange(client, wire::Call::default()).map_err(|cause| cause.cause())?.key;
    for value in [0, 1, 2, usize::MAX]
    {
        exchange(client,
                 wire::Call { operation: wire::SET,
                              key,
                              value,
                              ..Default::default() }).map_err(|cause| cause.cause())?;
        let actual = exchange(client,
                              wire::Call { operation: wire::GET,
                                           key,
                                           ..Default::default() }).map_err(|cause| cause.cause())?;
        assert_eq!(actual.value, value, "TLS opaque values, including std sentinels, round-trip");
    }
    exchange(client,
             wire::Call { operation: wire::DELETE,
                          key,
                          ..Default::default() }).map_err(|cause| cause.cause())?;
    let stale = exchange(client,
                         wire::Call { operation: wire::GET,
                                      key,
                                      ..Default::default() });
    assert!(matches!(stale,Err(cause) if cause.cause()==Error::NotFound),
            "deleted TLS key must not alias reuse");
    let mut keys = [0u64; 4096];
    let mut count = 0;
    loop
    {
        match exchange(client, wire::Call::default())
        {
            Ok(created) =>
            {
                assert!(count < keys.len(), "configured TLS capacity out of admitted range");
                keys[count] = created.key;
                count += 1;
            }
            Err(cause) =>
            {
                assert_eq!(cause.cause(),
                           Error::NoMemory,
                           "bounded TLS key exhaustion has typed NO_MEMORY and transport \
                            requires diagnostic");
                break;
            }
        }
    }
    for &key in &keys[..count]
    {
        exchange(client,
                 wire::Call { operation: wire::DELETE,
                              key,
                              ..Default::default() }).map_err(|cause| cause.cause())?;
    }
    let malformed = exchange(client,
                             wire::Call { flags: 1,
                                          ..Default::default() });
    assert!(matches!(malformed,Err(cause) if cause.cause()==Error::BadArgument),
            "unknown TLS flags refuse without breaking reuse");
    let created = exchange(client,
                           wire::Call { destructor: destructor as *const () as usize,
                                        ..Default::default() }).map_err(|cause| cause.cause())?;
    KEY.store(created.key, Ordering::Release);
    CLIENT.store(Box::leak(Box::new(client)), Ordering::Release);
    exchange(client,
             wire::Call { operation: wire::SET,
                          key: created.key,
                          value: 1,
                          ..Default::default() }).map_err(|cause| cause.cause())?;
    let mut text = crate::LogBuffer::new();
    writeln!(text,
             "TLS_PROBE_BODY current={actual} capacity={count} sentinels=true stale=true \
              reuse=true callback_queued=true").map_err(|_| Error::BadAbi)?;
    system.log(text.text())?;
    // The real platform application runtime performs BEGIN/NEXT/callback/ACK/
    // FINISH after return. Parent observes callbacks then real THRD/Process exit.
    Ok(())
}
