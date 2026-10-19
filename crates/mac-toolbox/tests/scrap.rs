//! Canonical Scrap Manager acceptance through the public aggregate Toolbox SDK.
#![cfg(not(target_arch = "riscv64"))]
#[allow(dead_code)]
mod conformance;
mod support;
use mac_toolbox::{BAD_ARGUMENT, Error, NO_MEMORY, System,
                  scrap::{MAX_BYTES, MAX_FLAVORS, TEXT}};
#[test]
fn original_scrap_trace_through_resident_userspace_services()
{
    support::with_system(|system| {
        assert_eq!(conformance::scrap::original_scrap(system).unwrap(), 18);
        assert_eq!(conformance::scrap::native_scrap(system).unwrap(), 17);
    });
}
#[test]
fn shared_oracle_matches_frozen_original_capture()
{
    let original: serde_json::Value =
        serde_json::from_str(include_str!("data/scrap/original.json")).unwrap();
    let records = original.as_array().unwrap();
    assert_eq!(records.len(), conformance::scrap::OBSERVATIONS.len());
    for (raw, expected) in records.iter().zip(conformance::scrap::OBSERVATIONS)
    {
        assert_eq!(raw["action"].as_str().unwrap(), expected.action);
        assert_eq!(raw["status"].as_i64().unwrap(), expected.status as i64);
        assert_eq!(raw["size"].as_u64().unwrap(), expected.size as u64);
        if expected.status >= 0 && raw["offset"].as_i64().unwrap() >= 0
        {
            assert_eq!(raw["offset"].as_u64().unwrap(), expected.offset as u64);
        }
        if expected.status > 0
        {
            let hex = raw["bytes"].as_str().unwrap();
            let bytes: Vec<u8> =
                hex.as_bytes()
                   .as_chunks::<2>()
                   .0
                   .iter()
                   .map(|pair| u8::from_str_radix(core::str::from_utf8(pair).unwrap(), 16).unwrap())
                   .collect();
            assert_eq!(bytes, expected.bytes);
        }
    }
}
#[test]
fn byte_and_record_limits_preserve_the_last_successful_clipboard()
{
    support::with_system(|system| {
        system.scrap().clear().unwrap();
        system.scrap().put_text("retained").unwrap();
        let before = system.scrap().info().unwrap();
        let excessive = vec![0xab; MAX_BYTES];
        assert_eq!(system.scrap().put(*b"SIZE", &excessive), Err(Error(NO_MEMORY)));
        assert_eq!(system.scrap().info().unwrap(), before);
        let mut text = [0; 32];
        assert_eq!(system.scrap().text(&mut text).unwrap(), "retained");
        system.scrap().clear().unwrap();
        for _ in 0..MAX_FLAVORS
        {
            system.scrap().put(*b"ZERO", &[]).unwrap();
        }
        let before = system.scrap().info().unwrap();
        assert_eq!(before.size as usize, MAX_FLAVORS * 8);
        assert_eq!(system.scrap().put(*b"LAST", &[]), Err(Error(NO_MEMORY)));
        assert_eq!(system.scrap().info().unwrap(), before);
        system.scrap().clear().unwrap();
        system.scrap().put(*b"FULL", &excessive[..MAX_BYTES - 8]).unwrap();
        assert_eq!(system.scrap().info().unwrap().size as usize, MAX_BYTES);
        let before = system.scrap().info().unwrap();
        assert_eq!(system.scrap().put(*b"ZERO", &[]), Err(Error(NO_MEMORY)));
        assert_eq!(system.scrap().info().unwrap(), before);
        system.scrap().clear().unwrap();
        system.scrap().put_text("capacity recovered").unwrap();
        assert_eq!(system.scrap().text(&mut text).unwrap(), "capacity recovered");
    });
}
#[test]
fn transport_clears_borrowed_pointers_on_success_and_failure()
{
    use mac_abi::scrap::{PUT, ScrapCall};
    support::with_system(|system| {
        let mut call = ScrapCall { operation: PUT,
                                   kind: *b"BYTE",
                                   input: usize::MAX as *const u8,
                                   input_length: usize::MAX,
                                   output: usize::MAX as *mut u8,
                                   capacity: usize::MAX,
                                   ..Default::default() };
        system.client().scrap(&mut call, b"ABCD", &mut []).unwrap();
        assert!(call.input.is_null() && call.output.is_null());
        assert_eq!((call.input_length, call.capacity), (0, 0));
        call.kind = TEXT;
        assert_eq!(system.client().scrap(&mut call, b"bypass", &mut []), Err(Error(BAD_ARGUMENT)));
        assert!(call.input.is_null() && call.output.is_null());
        assert_eq!((call.input_length, call.capacity), (0, 0));
    });
}
#[test]
fn host_threads_share_one_clipboard_without_torn_payloads_or_lost_records()
{
    use std::{sync::{Barrier,
                     atomic::{AtomicUsize, Ordering}},
              thread,
              time::{Duration, Instant}};
    fn retry<T>(busy: &AtomicUsize, mut operation: impl FnMut() -> Result<T, Error>) -> T
    {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop
        {
            match operation()
            {
                Ok(value) => return value,
                Err(Error(mac_toolbox::SERVICE_BUSY)) =>
                {
                    busy.fetch_add(1, Ordering::Relaxed);
                    assert!(Instant::now() < deadline, "shared Scrap service made no progress");
                    thread::yield_now();
                }
                Err(error) => panic!("unexpected Scrap error: {error:?}")
            }
        }
    }
    mac_system::testing::scrap::with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        system.scrap().clear().unwrap();
        let generation = system.scrap().info().unwrap().count;
        let barrier = Barrier::new(8);
        let busy = AtomicUsize::new(0);
        thread::scope(|scope| {
            for worker in 1..=8u8
            {
                let system = &system;
                let barrier = &barrier;
                let busy = &busy;
                scope.spawn(move || {
                         barrier.wait();
                         let pattern = [worker; 4096];
                         let kind = [b'T', b'H', b'R', worker];
                         for _ in 0..16
                         {
                             retry(busy, || system.scrap().put(kind, &pattern));
                             let mut output = [0u8; 4096];
                             assert_eq!(retry(busy, || system.scrap().read(kind, 0, &mut output)),
                                        4096);
                             assert_eq!(output, pattern);
                         }
                     });
            }
        });
        let info = system.scrap().info().unwrap();
        assert_eq!(info.flavors, 128);
        assert_eq!(info.size, 128 * (4096 + 8));
        assert_eq!(info.count, generation);
        for index in 0..128
        {
            let flavor = system.scrap().flavor_at(index).unwrap();
            assert_eq!(&flavor.kind[..3], b"THR");
            assert!((1..=8).contains(&flavor.kind[3]));
            assert_eq!(flavor.length, 4096);
            assert_eq!(flavor.offset, index as u32 * (4096 + 8) + 8);
        }
        // Contention count is diagnostic only; correctness must not depend on
        // the host scheduler choosing a specific overlapping execution trace.
        let _ = busy.load(Ordering::Relaxed);
    });
}
#[test]
fn pascal_text_traps_convert_at_the_boundary_and_preserve_full_native_unicode()
{
    mac_system::testing::with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        assert_eq!(conformance::scrap::legacy_text(&system, services.trap).unwrap(), 12);
    });
}
