//! Safe owned module SDK acceptance through the real phase-5 broker seam.
//! Guard Drop only retires records; bounded drains own cleanup acknowledgments.
#![cfg(not(target_arch = "riscv64"))]
#[path = "support/allocations.rs"]
mod allocations;
use allocations::Observe;
use mac_driver_alloc::scrap::Scrap;
use mac_system::testing::scrap_extension::{self as fixture, ReplyFault};
use mac_toolbox::System;

fn finish(client: &mut Scrap)
{
    for _ in 0..32
    {
        let report = client.drain(1);
        assert!(report.failure.is_none(), "bounded SCEX drain failed: {:?}", report.failure);
        assert!(report.visited <= 1, "drain exceeded its inspected-record budget");
        if report.pending == 0
        {
            return;
        }
    }
    panic!("bounded SCEX drains retained {} records", client.pending());
}

#[test]
fn guard_drop_allocates_nothing_and_live_snapshot_keeps_its_session_usable()
{
    fixture::with_services(|services, fixture| {
        // SAFETY: fixture pins its real public Scrap table throughout this scope.
        let system = unsafe { System::from_raw(services) }.unwrap();
        system.scrap().put_text("owned snapshot Ω").unwrap();
        let module = fixture.extension(true, true);
        module.with_client(|entry| {
                  let mut client = Scrap::new(entry).unwrap();
                  let session = client.open(true).unwrap();
                  let snapshot = client.snapshot(&session).unwrap();
                  let observed = Observe::begin(true, None);
                  drop(session);
                  let allocated = observed.finish();
                  assert_eq!(allocated, 0, "Session Drop allocated cleanup bookkeeping");
                  for _ in 0..8
                  {
                      let report = client.drain(1);
                      assert!(report.visited <= 1);
                      assert_eq!(report.completed, 0,
                                 "retiring parent disconnected a live snapshot");
                  }
                  assert_eq!(client.info(&snapshot).unwrap().bytes,
                             "owned snapshot Ω".len() as u32);
                  let observed = Observe::begin(true, None);
                  drop(snapshot);
                  let allocated = observed.finish();
                  assert_eq!(allocated, 0, "Snapshot Drop allocated or dispatched cleanup");
                  assert_eq!(client.pending(), 2);
                  finish(&mut client);
                  client.shutdown(1).ok().expect("acknowledged empty SDK shuts down");
              });
        let released = module.cleanup(false, false);
        assert_eq!((released.summary.live, released.summary.pending), (0, 0));
    });
}

#[test]
fn real_busy_cleanup_retains_same_grants_until_acknowledged_retry()
{
    fixture::with_services(|_, fixture| {
        let module = fixture.extension(true, true);
        module.with_client(|entry| {
                  let mut client = Scrap::new(entry).unwrap();
                  let before_creation = module.summary();
                  let busy = module.with_busy(|| client.open(true))
                                   .err()
                                   .expect("busy OPEN must refuse before effect");
                  assert!(busy.is_busy() && busy.to_string().contains("retry"), "{busy}");
                  assert_eq!((client.pending(), client.uncertain()), (0, 0));
                  assert_eq!(module.summary().live, before_creation.live);
                  let session = client.open(true).unwrap();
                  let before_snapshot = module.summary();
                  let busy = module.with_busy(|| client.snapshot(&session))
                                   .err()
                                   .expect("busy SNAPSHOT must refuse before effect");
                  assert!(busy.is_busy(), "{busy}");
                  assert_eq!((client.pending(), client.uncertain()), (1, 0));
                  assert_eq!(module.summary().live, before_snapshot.live);
                  let snapshot = client.snapshot(&session).unwrap();
                  drop(session);
                  drop(snapshot);
                  let before = module.summary();
                  let report = module.with_busy(|| client.drain(4));
                  assert!(report.busy > 0, "real resident contention was not reported");
                  assert!(report.failure.as_ref().is_some_and(|cause| cause.is_busy()));
                  assert_eq!(report.completed, 0, "Busy falsely acknowledged cleanup");
                  assert_eq!(report.pending, 2, "Busy lost the original session/snapshot grant");
                  let after = module.summary();
                  assert_eq!((after.live, after.pending), (before.live, before.pending));
                  finish(&mut client);
                  let fresh = client.open(true).unwrap();
                  drop(fresh);
                  finish(&mut client);
              });
        let released = module.cleanup(false, false);
        assert_eq!((released.summary.live, released.summary.pending), (0, 0));
    });
}

#[test]
fn allocation_refusal_precedes_open_and_snapshot_effects()
{
    fixture::with_services(|_, fixture| {
        let module = fixture.extension(true, true);
        module.with_client(|entry| {
                  let mut client = Scrap::new(entry).unwrap();
                  let before = module.summary();
                  let observed = Observe::fail_at(1);
                  let error = client.open(true).err();
                  let allocations = observed.finish();
                  let error = error.expect("OPEN must refuse failed owned-record admission");
                  assert!(allocations > 0);
                  assert!(error.to_string().to_lowercase().contains("memory"), "{error}");
                  assert_eq!(client.pending(), 0);
                  assert_eq!(module.summary().live,
                             before.live,
                             "failed preflight opened resident session");
                  let session = client.open(true).unwrap();
                  let before = module.summary();
                  let observed = Observe::begin(true, None);
                  let error = client.snapshot(&session).err();
                  let allocations = observed.finish();
                  let error = error.expect("SNAPSHOT must refuse failed owned-record admission");
                  assert!(allocations > 0);
                  assert!(error.to_string().to_lowercase().contains("memory"), "{error}");
                  assert_eq!(client.pending(), 1);
                  assert_eq!(module.summary().live,
                             before.live,
                             "failed preflight pinned resident snapshot");
                  drop(session);
                  finish(&mut client);
              });
        let released = module.cleanup(false, false);
        assert_eq!((released.summary.live, released.summary.pending), (0, 0));
    });
}

#[test]
fn malformed_real_creation_reply_retains_uncertainty_and_shutdown_client()
{
    for fault in [ReplyFault::Oversize, ReplyFault::InvalidEnvelope]
    {
        fixture::with_services(|_, fixture| {
            let module = fixture.extension(true, true);
            module.with_client(|entry| {
                      let mut client = Scrap::new(entry).unwrap();
                      let before = module.summary();
                      module.corrupt_next_reply(fault);
                      let error = client.open(true)
                                        .err()
                                        .expect("corrupted real OPEN reply must be refused");
                      assert!(!error.to_string().is_empty());
                      assert_eq!(client.pending(),
                                 1,
                                 "post-effect failure lost its reserved grant");
                      assert_eq!(client.uncertain(), 1);
                      assert!(module.summary().live > before.live,
                              "fault did not follow an actual OPEN effect");
                      let stopped = client.shutdown(8)
                                          .err()
                                          .expect("uncertain creation cannot claim shutdown");
                      assert_eq!(stopped.report().pending, 1);
                      let mut client = stopped.into_client();
                      let report = client.drain(8);
                      assert_eq!(report.completed, 0,
                                 "uncertain grant was fabricated or acknowledged");
                      assert_eq!(report.uncertain, 1);
                  });
            // Only the actual owner walk can release a handle whose malformed
            // creation reply prevented the SDK from learning its identity.
            let released = module.cleanup(false, false);
            assert_eq!((released.summary.live, released.summary.pending), (0, 0));
        });
    }
}

#[test]
fn quiesced_owner_never_becomes_a_successful_guard_drain()
{
    fixture::with_services(|_, fixture| {
        let module = fixture.extension(true, true);
        module.with_client(|entry| {
                  let mut client = Scrap::new(entry).unwrap();
                  let session = client.open(true).unwrap();
                  let snapshot = client.snapshot(&session).unwrap();
                  let first = module.cleanup(false, false);
                  assert!(first.summary.pending > 0,
                          "live admitted client root disappeared during walk");
                  drop(snapshot);
                  drop(session);
                  let report = client.drain(8);
                  assert_eq!(report.completed, 0);
                  assert_eq!(report.pending, 2,
                             "quiesced owner fabricated resident cleanup acknowledgment");
                  let stopped = client.shutdown(8)
                                      .err()
                                      .expect("quiesced provider cannot acknowledge shutdown");
                  assert_eq!(stopped.report().pending, 2);
                  drop(stopped.into_client());
              });
        let finished = module.cleanup(false, true);
        assert_eq!((finished.summary.live, finished.summary.pending, finished.summary.quarantined),
                   (0, 0, 0));
    });
}

#[test]
fn malformed_snapshot_reply_retains_unknown_child_and_its_original_parent()
{
    fixture::with_services(|_, fixture| {
        let module = fixture.extension(true, true);
        module.with_client(|entry| {
                  let mut client = Scrap::new(entry).unwrap();
                  let session = client.open(true).unwrap();
                  let before = module.summary();
                  module.corrupt_next_reply(ReplyFault::InvalidEnvelope);
                  let error =
                      client.snapshot(&session).err().expect("corrupted real SNAPSHOT must fail");
                  assert!(error.to_string().contains("SNAPSHOT"), "{error}");
                  assert_eq!((client.pending(), client.uncertain()), (2, 1));
                  assert!(module.summary().live > before.live);
                  drop(session);
                  let report = client.drain(16);
                  assert_eq!(report.completed, 0, "unknown live child permitted parent disconnect");
                  assert_eq!((report.pending, report.uncertain), (2, 1));
              });
        let released = module.cleanup(false, false);
        assert_eq!((released.summary.live, released.summary.pending), (0, 0));
    });
}

#[test]
fn foreign_client_guards_retain_original_provider_and_cause()
{
    fixture::with_services(|_, fixture| {
        let module = fixture.extension(true, true);
        module.with_client(|entry| {
                  let mut original = Scrap::new(entry).unwrap();
                  let mut foreign = Scrap::new(entry).unwrap();
                  let session = original.open(true).unwrap();
                  let snapshot = original.snapshot(&session).unwrap();
                  let before = module.summary();
                  let error =
                      foreign.snapshot(&session).err().expect("foreign parent must be refused");
                  assert_eq!(error.status(), mac_abi::ACCESS_DENIED);
                  assert!(error.to_string().contains("another client")
                          && error.to_string().contains("original"),
                          "{error}");
                  assert_eq!(foreign.info(&snapshot).unwrap_err().status(), mac_abi::ACCESS_DENIED);
                  assert_eq!(foreign.notice(&session).unwrap_err().status(),
                             mac_abi::ACCESS_DENIED);
                  assert_eq!(foreign.pending(), 0);
                  assert_eq!(module.summary().live, before.live);
                  original.info(&snapshot).unwrap();
                  drop(session);
                  drop(snapshot);
                  finish(&mut original);
                  foreign.shutdown(1).ok().expect("empty foreign SDK shuts down");
              });
        let released = module.cleanup(false, false);
        assert_eq!((released.summary.live, released.summary.pending), (0, 0));
    });
}

#[test]
fn cleanup_budget_bounds_large_backlog_and_swap_removal_skips_no_grant()
{
    const SESSIONS: usize = 32;
    const GRANTS: usize = SESSIONS * 2;
    fixture::with_services(|_, fixture| {
        let module = fixture.extension(true, true);
        module.with_client(|entry| {
                  let mut client = Scrap::new(entry).unwrap();
                  let mut sessions = Vec::new();
                  let mut snapshots = Vec::new();
                  for _ in 0..SESSIONS
                  {
                      let session = client.open(true).unwrap();
                      snapshots.push(client.snapshot(&session).unwrap());
                      sessions.push(session);
                  }
                  assert_eq!(client.pending(), GRANTS);
                  // A budget of one inspects one live record even with a backlog
                  // much larger than that budget. Shutdown preserves the exact client.
                  let stopped =
                      client.shutdown(1).err().expect("live guards retain shutdown ownership");
                  assert_eq!(stopped.report().visited, 1);
                  assert_eq!(stopped.report().attempted, 0);
                  assert_eq!(stopped.report().pending, GRANTS);
                  let mut client = stopped.into_client();
                  drop(sessions);
                  drop(snapshots);
                  let mut completed = 0;
                  for _ in 0..GRANTS * 8
                  {
                      let report = client.drain(1);
                      assert!(report.visited <= 1
                              && report.attempted <= 1
                              && report.completed <= 1,
                              "one-record drain exceeded its budget: {report:?}");
                      assert!(report.failure.is_none(), "{report:?}");
                      completed += report.completed;
                      if report.pending == 0
                      {
                          break;
                      }
                  }
                  assert_eq!(completed, GRANTS, "bounded removal skipped an admitted grant");
                  assert_eq!(client.pending(), 0);
                  client.shutdown(1).ok().expect("acknowledged backlog shuts down");
              });
        let released = module.cleanup(false, false);
        assert_eq!((released.summary.live, released.summary.pending, released.summary.quarantined),
                   (0, 0, 0));
    });
}
