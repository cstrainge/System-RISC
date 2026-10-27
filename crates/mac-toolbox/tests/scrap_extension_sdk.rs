//! Canonical public module SDK acceptance through the same phase-5 core used by
//! native System. Host execution qualifies neither IRQ discipline nor workers.
#![cfg(not(target_arch = "riscv64"))]
use mac_driver::{Client, Provider};
use mac_system::testing::scrap_extension::{self as fixture, wire};
use mac_toolbox::System;

fn provider(client: Client) -> Provider
{
    client.resolve(wire::CLASS, wire::VERSION).unwrap()
}
fn call(provider: Provider, operation: u32, input: &[u8]) -> Vec<u8>
{
    let mut reply = [0; wire::MIN_REPLY];
    let length = provider.synchronous(operation, input, &mut reply).unwrap();
    match wire::Reply::decode(&reply[..length]).expect("resident SCEX reply is a valid envelope")
    {
        wire::Reply::Success(bytes) => bytes.to_vec(),
        wire::Reply::Failure { status,
                               cause } => panic!("SCEX refused ({status}): {cause}")
    }
}
fn open(provider: Provider) -> [u8; 8]
{
    call(provider, wire::OPEN, &1u32.to_le_bytes()).try_into().unwrap()
}

#[test]
fn entry_client_resolves_actual_ready_init_and_phase_five_has_no_hidden_ticket()
{
    fixture::with_services(|services, fixture| {
        // SAFETY: this scope pins the fixture's real public Scrap service table.
        let system = unsafe { System::from_raw(services) }.unwrap();
        system.scrap().put_text("public module SDK Ω").unwrap();
        let module = fixture.extension(true, true);
        module.with_client(|client| {
                  assert_ne!(client.module(), 0);
                  let provider = provider(client);
                  let before = module.summary();
                  let mut reply = [0; wire::MIN_REPLY];
                  let mut ticket = 0;
                  assert_eq!(provider.invoke(&mut ticket,
                                             mac_driver::Phase::Start,
                                             wire::OPEN,
                                             &1u32.to_le_bytes(),
                                             &mut reply),
                             Err(mac_driver::Error(mac_abi::BAD_ARGUMENT)));
                  assert_eq!(ticket, 0, "phase refusal published a broker ticket");
                  assert_eq!(module.summary().live, before.live, "phase refusal opened a session");
                  let busy =
                      module.with_busy(|| {
                                provider.synchronous(wire::OPEN, &1u32.to_le_bytes(), &mut reply)
                            });
                  assert_eq!(busy, Err(mac_driver::Error(mac_abi::SERVICE_BUSY)));
                  assert_eq!(module.summary().live, before.live, "Busy created hidden owned state");
                  assert_eq!(module.summary().pending, before.pending);
                  let session = open(provider);
                  let pin = call(provider, wire::SNAPSHOT, &session);
                  let mut pair = session.to_vec();
                  pair.extend_from_slice(&pin);
                  let info = call(provider, wire::INFO, &pair);
                  assert_eq!(wire::Metadata::decode(&info).unwrap().bytes,
                             "public module SDK Ω".len() as u32);
                  pair.extend_from_slice(&0u64.to_le_bytes());
                  assert_eq!(call(provider, wire::READ, &pair), "public module SDK Ω".as_bytes());
                  call(provider, wire::CLOSE_PIN, &pair[..16]);
                  call(provider, wire::DISCONNECT, &session);
                  let length = provider.synchronous(wire::NOTICE, &session, &mut reply).unwrap();
                  let Some(wire::Reply::Failure { status,
                                                  cause }) = wire::Reply::decode(&reply[..length])
                  else
                  {
                      panic!("stale session received no diagnostic failure envelope");
                  };
                  assert_eq!(status, mac_abi::NOT_FOUND);
                  assert!(cause.contains("disconnected") && cause.contains("fresh"), "{cause}");
              });
        let released = module.cleanup(false, false);
        assert_eq!((released.summary.live, released.summary.pending, released.summary.quarantined),
                   (0, 0, 0));
    });
}

#[test]
fn closed_external_subject_refuses_cached_provider_before_any_creation()
{
    fixture::with_services(|_, fixture| {
        let module = fixture.extension(true, true);
        module.with_client(|client| {
                  let provider = provider(client);
                  module.close_client_root();
                  let before = module.summary();
                  let mut output = [0; wire::MIN_REPLY];
                  assert_eq!(provider.synchronous(wire::OPEN, &1u32.to_le_bytes(), &mut output),
                             Err(mac_driver::Error(mac_abi::SERVICE_BUSY)));
                  let after = module.summary();
                  assert_eq!((after.live, after.pending),
                             (before.live, before.pending),
                             "closed external root admitted session or request ownership");
              });
        let released = module.cleanup(false, false);
        assert_eq!((released.summary.live, released.summary.pending), (0, 0));
    });
}

#[test]
fn cached_client_and_provider_fail_closed_after_scope_and_fixture_teardown()
{
    let (client, cached) = fixture::with_services(|_, fixture| {
        let module = fixture.extension(true, true);
        let cached = module.with_client(|client| (client, provider(client)));
        assert_eq!(cached.0.resolve(wire::CLASS, wire::VERSION).err(),
                   Some(mac_driver::Error(mac_abi::ACCESS_DENIED)));
        let mut output = [0; wire::MIN_REPLY];
        assert_eq!(cached.1.synchronous(wire::OPEN, &0u32.to_le_bytes(), &mut output),
                   Err(mac_driver::Error(mac_abi::ACCESS_DENIED)));
        cached
    });
    assert_eq!(client.resolve(wire::CLASS, wire::VERSION).err(),
               Some(mac_driver::Error(mac_abi::NOT_SUPPORTED)));
    let mut output = [0; wire::MIN_REPLY];
    assert_eq!(cached.synchronous(wire::OPEN, &0u32.to_le_bytes(), &mut output),
               Err(mac_driver::Error(mac_abi::NOT_SUPPORTED)));
    // A new fixture must not recycle an expired context into another INIT.
    fixture::with_services(|_, fixture| {
        let replacement = fixture.extension(true, true);
        replacement.with_client(|fresh| {
                       assert_eq!(cached.synchronous(wire::OPEN, &0u32.to_le_bytes(), &mut output),
                                  Err(mac_driver::Error(mac_abi::ACCESS_DENIED)));
                       let session = open(provider(fresh));
                       call(provider(fresh), wire::DISCONNECT, &session);
                   });
    });
}

#[test]
fn undeclared_and_driver_clients_preserve_resident_diagnostic_causes()
{
    fixture::with_services(|_, fixture| {
        let undeclared = fixture.extension(false, true);
        undeclared.with_client(|client| {
                      assert_eq!(client.resolve(wire::CLASS, wire::VERSION).err(),
                                 Some(mac_driver::Error(mac_abi::ACCESS_DENIED)));
                  });
        let driver = fixture.driver();
        driver.with_client(|client| {
                  let provider = provider(client);
                  let mut reply = [0; wire::MIN_REPLY];
                  let before = driver.summary();
                  let length =
                      provider.synchronous(wire::OPEN, &1u32.to_le_bytes(), &mut reply).unwrap();
                  let Some(wire::Reply::Failure { status,
                                                  cause }) = wire::Reply::decode(&reply[..length])
                  else
                  {
                      panic!("non-INIT client did not receive a typed resident cause");
                  };
                  assert_eq!(status, mac_abi::ACCESS_DENIED);
                  assert!(cause.contains("INIT") && cause.contains("driver"), "{cause}");
                  assert_eq!(driver.summary().live, before.live);
              });
    });
}
