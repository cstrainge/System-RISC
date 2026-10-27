use mac_virtio_console::{Diagnostic,
                         discovery::{DISCOVERY_MICROSECONDS, Discovery, Step}};
#[test]
fn pre_dispatch_probe_contention_retries_the_same_ordinal_without_renewing_deadline()
{
    let mut search = Discovery::new(3);
    assert_eq!(search.step(100), Step::Probe(0));
    search.probe_busy(100);
    assert_eq!(search.step(101), Step::Wait);
    assert_eq!(search.step(1100), Step::Probe(0));
    search.probe_busy(1100);
    assert_eq!(search.step(2100), Step::Probe(0));
    search.probed();
    assert_eq!(search.step(2101), Step::Probe(1));
    assert_eq!(search.step(100 + DISCOVERY_MICROSECONDS), Step::Refused(Diagnostic::Claimed));
}
#[test]
fn an_unrelated_claimed_controller_cannot_keep_absent_named_discovery_alive_forever()
{
    let mut search = Discovery::new(2);
    assert_eq!(search.step(0), Step::Probe(0));
    search.probed();
    search.claim_busy();
    assert_eq!(search.step(1), Step::Probe(1));
    search.probed();
    assert_eq!(search.step(2), Step::Wait);
    assert_eq!(search.step(3), Step::Wait);
    assert_eq!(search.step(1002), Step::Probe(0));
    search.probed();
    search.claim_busy();
    assert_eq!(search.step(DISCOVERY_MICROSECONDS), Step::Refused(Diagnostic::Claimed));
    assert!(Diagnostic::Claimed.message().contains("restart System"));
}
#[test]
fn empty_and_fully_examined_inventories_finish_without_extra_probe_work()
{
    assert_eq!(Discovery::new(0).step(7), Step::Exhausted);
    let mut search = Discovery::new(1);
    assert_eq!(search.step(7), Step::Probe(0));
    search.probed();
    assert_eq!(search.step(8), Step::Exhausted);
}
#[test]
fn unnamed_candidate_control_and_reset_time_share_the_original_inventory_budget()
{
    let mut search = Discovery::new(8);
    assert_eq!(search.step(50), Step::Probe(0));
    search.probed();
    assert!(!search.expired(49 + DISCOVERY_MICROSECONDS));
    assert!(search.expired(50 + DISCOVERY_MICROSECONDS));
    assert_eq!(search.timeout_diagnostic(), Diagnostic::DiscoveryTimeout);
    assert!(search.timeout_diagnostic().message().contains("controller name and profile"));
}
#[test]
fn identified_but_host_closed_port_waits_for_open_and_real_reset_allows_a_new_budget()
{
    let mut search = Discovery::new(4);
    assert_eq!(search.step(0), Step::Probe(0));
    search.probed();
    search.identified();
    assert!(!search.expired(u64::MAX));
    search.reconnect(2);
    assert_eq!(search.step(10_000_000), Step::Probe(2));
    assert!(!search.expired(10_000_000 + DISCOVERY_MICROSECONDS - 1));
    assert!(search.expired(10_000_000 + DISCOVERY_MICROSECONDS));
}
