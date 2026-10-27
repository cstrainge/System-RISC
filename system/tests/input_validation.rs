//! Native driver predicates, before MMIO values become array indices or DMA.
#[path = "../src/machine/input/validation.rs"]
mod validation;
#[path = "../../crates/mac-toolbox/tests/support/allocations.rs"]
mod allocations;

use allocations::Observe;
use validation::{DEVICE_COUNT, EVENT_BYTES, Error, QUEUE_LEN};

const BASE: usize = 0x1000_1000;

#[test]
fn malformed_completion_is_rejected_before_an_event_index_exists()
{
    let observed = Observe::begin(true, None);
    let valid = validation::completion(BASE, (QUEUE_LEN - 1) as u32, EVENT_BYTES, 0);
    let out_of_range = validation::completion(BASE, QUEUE_LEN as u32, EVENT_BYTES, 0);
    let huge = validation::completion(BASE, u32::MAX, EVENT_BYTES, 0);
    let short = validation::completion(BASE, 0, EVENT_BYTES - 1, 0);
    let long = validation::completion(BASE, 0, EVENT_BYTES + 1, 0);
    let duplicate = validation::completion(BASE, 3, EVENT_BYTES, 1 << 3);
    let other_endpoint = validation::completion(BASE + 0x1000, 3, EVENT_BYTES, 0);
    let count = observed.finish();
    assert_eq!(count, 0);
    assert_eq!(valid, Ok(QUEUE_LEN - 1));
    assert_eq!(other_endpoint, Ok(3), "one bad endpoint must not poison the other's descriptors");
    for (cause, id, bytes) in [(out_of_range, QUEUE_LEN as u32, EVENT_BYTES),
                               (huge, u32::MAX, EVENT_BYTES),
                               (short, 0, EVENT_BYTES - 1),
                               (long, 0, EVENT_BYTES + 1)]
    {
        let error = cause.unwrap_err();
        assert_eq!(error,
                   Error::Completion { base: BASE,
                                       id,
                                       bytes });
        let text = error.to_string();
        assert!(text.contains("10001000") && text.contains("before reading event storage"),
                "{text}");
    }
    assert_eq!(duplicate,
               Err(Error::Duplicate { base: BASE,
                                      id:   3 }));
    assert!(duplicate.unwrap_err().to_string().contains("before recycling was published"));
}

#[test]
fn used_index_wrap_is_valid_but_device_overrun_cannot_select_a_ring_entry()
{
    let observed = Observe::begin(true, None);
    let empty = validation::used_count(BASE, 42, 42);
    let full = validation::used_count(BASE, 42, 42 + QUEUE_LEN as u16);
    let wrapped = validation::used_count(BASE, u16::MAX - 2, 2);
    let overflow = validation::used_count(BASE, 42, 43 + QUEUE_LEN as u16);
    let backwards = validation::used_count(BASE, 42, 41);
    let count = observed.finish();
    assert_eq!(count, 0);
    assert_eq!(empty, Ok(0));
    assert_eq!(full, Ok(QUEUE_LEN));
    assert_eq!(wrapped, Ok(5));
    for refused in [overflow, backwards]
    {
        let cause = refused.unwrap_err();
        assert!(matches!(cause,
                         Error::UsedAdvance { base: BASE,
                                              .. }));
        assert!(cause.to_string().contains("admitted descriptors"));
    }
}

#[test]
fn device_capability_failures_are_typed_and_precede_queue_publication()
{
    let observed = Observe::begin(true, None);
    let modern = validation::identity(BASE, 0x7472_6976, 2, 18);
    let old = validation::identity(BASE, 0x7472_6976, 1, 18);
    let wrong = validation::identity(BASE, 0x7472_6976, 2, 2);
    let feature = validation::version_feature(BASE, 0);
    let accepted = validation::features(BASE, 11);
    let features = [0, 3, 11 | 64, 11 | 128].map(|status| validation::features(BASE, status));
    let ready = validation::queue(BASE, 1, QUEUE_LEN as u32);
    let small = validation::queue(BASE, 0, (QUEUE_LEN - 1) as u32);
    let enough = validation::queue(BASE, 0, QUEUE_LEN as u32);
    let axis = validation::axis(BASE, 0, 0, 32767);
    let bad_axis = validation::axis(BASE, 1, 1, 65535);
    let count = observed.finish();
    assert_eq!(count, 0);
    assert_eq!(modern, Ok(()));
    assert_eq!(accepted, Ok(()));
    assert_eq!(enough, Ok(()));
    assert_eq!(axis, Ok(()));
    assert!(matches!(old,
                     Err(Error::Identity { version: 1,
                                           .. })));
    assert!(matches!(wrong,
                     Err(Error::Identity { device: 2,
                                           .. })));
    assert_eq!(feature, Err(Error::VersionFeature { base: BASE }));
    for cause in features.into_iter().chain([feature, small]).map(Result::unwrap_err)
    {
        assert!(cause.to_string().contains("queue was not published"));
    }
    assert_eq!(ready,
               Err(Error::QueueReady { base:  BASE,
                                       ready: 1 }));
    assert!(ready.unwrap_err().to_string().contains("queue was not replaced"));
    assert!(matches!(bad_axis,
                     Err(Error::Axis { axis: 1,
                                       minimum: 1,
                                       maximum: 65535,
                                       .. })));
    assert!(bad_axis.unwrap_err().to_string().contains("expected 0..32767"));
}

#[test]
fn terminal_admission_and_retirement_diagnostics_preserve_the_failure()
{
    let cases = [(Error::Dma { buffer: "events",
                               cpu:    4096,
                               bytes:  512 },
                  "contiguous bus mapping"),
                 (Error::Reset { base:   BASE,
                                 status: 15 },
                  "retained buffers will not be reused"),
                 (Error::DeviceFailed { base:   BASE,
                                        status: 64 },
                  "buffers pinned"),
                 (Error::DeviceCount { found: DEVICE_COUNT - 1 }, "platform requires 2"),
                 (Error::NotPrepared, "not admitted during bootstrap"),
                 (Error::AlreadyPrepared, "original prepared storage"),
                 (Error::AlreadyTaken, "normal input must not resume")];
    for (cause, expected) in cases
    {
        assert!(cause.to_string().contains(expected), "{cause}");
    }
}

#[test]
fn terminal_keeps_the_healthy_peer_and_reports_each_retirement_once_without_allocation()
{
    use validation::TerminalEndpoints;
    let rejected = Error::Reset { base:   BASE,
                                  status: 15 };
    let failed = Error::DeviceFailed { base:   BASE + 0x1000,
                                       status: 64 };
    let later = Error::Duplicate { base: BASE + 0x1000,
                                   id:   3 };
    let observed = Observe::begin(true, None);
    let mut endpoints = TerminalEndpoints::from_admission([Err(rejected), Ok(())]);
    let usable_after_partial_admission = endpoints.has_live_device();
    let peer_is_usable = !endpoints.is_live(0) && endpoints.is_live(1);
    let admission_errors = endpoints.take_errors();
    let admission_errors_again = endpoints.take_errors();
    endpoints.retire(1, failed);
    endpoints.retire(1, later);
    let usable_after_retirement = endpoints.has_live_device();
    let retirement_errors = endpoints.take_errors();
    let retirement_errors_again = endpoints.take_errors();
    let allocations = observed.finish();
    assert_eq!(allocations, 0);
    assert!(usable_after_partial_admission && peer_is_usable);
    assert_eq!(admission_errors, [Some(rejected), None]);
    assert_eq!(admission_errors_again, [None, None]);
    assert!(!usable_after_retirement, "the terminal must stop rather than await dead input");
    assert_eq!(retirement_errors, [None, Some(failed)], "keep the original retirement cause");
    assert_eq!(retirement_errors_again, [None, None]);
    assert!(failed.to_string().contains("10002000"));
}

#[test]
fn losing_both_terminal_devices_reports_both_causes_before_unavailable_shutdown()
{
    use validation::TerminalEndpoints;
    let first = Error::DeviceFailed { base:   BASE,
                                      status: 128 };
    let second = Error::Completion { base:  BASE + 0x1000,
                                     id:    QUEUE_LEN as u32,
                                     bytes: EVENT_BYTES };
    let observed = Observe::begin(true, None);
    let mut endpoints = TerminalEndpoints::from_admission([Ok(()); DEVICE_COUNT]);
    let initially_usable = endpoints.has_live_device();
    endpoints.retire(0, first);
    let peer_survives = endpoints.has_live_device() && endpoints.is_live(1);
    endpoints.retire(1, second);
    let all_retired = !endpoints.has_live_device();
    let errors = endpoints.take_errors();
    let repeated = endpoints.take_errors();
    let mut rejected = TerminalEndpoints::from_admission([Err(first), Err(second)]);
    let admission_unavailable = !rejected.has_live_device();
    let refused_causes = rejected.take_errors();
    let allocations = observed.finish();
    assert_eq!(allocations, 0);
    assert!(initially_usable && peer_survives && all_retired && admission_unavailable);
    assert_eq!(errors, [Some(first), Some(second)]);
    assert_eq!(repeated, [None, None]);
    assert_eq!(refused_causes, errors);
    assert!(second.to_string().contains("before reading event storage"));
}
