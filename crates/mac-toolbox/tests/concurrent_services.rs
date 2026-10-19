//! Native concurrency contracts: actual public SDK calls enter one shared real
//! resident manager using the same nonwaiting holder as the kernel. Host threads
//! prove shared-manager exclusion; they do not establish an RV64 OS scheduler.
#![cfg(not(target_arch = "riscv64"))]
use std::{sync::{Barrier,
                 atomic::{AtomicUsize, Ordering}},
          thread,
          time::{Duration, Instant}};

use mac_toolbox::{Error, SERVICE_BUSY, System};

fn retry<T>(busy: &AtomicUsize, mut call: impl FnMut() -> Result<T, Error>) -> T
{
    let deadline = Instant::now() + Duration::from_secs(10);
    loop
    {
        match call()
        {
            Ok(value) => return value,
            Err(Error(SERVICE_BUSY)) =>
            {
                busy.fetch_add(1, Ordering::Relaxed);
                assert!(Instant::now() < deadline, "resident shared service made no progress");
                thread::yield_now();
            }
            Err(error) => panic!("unexpected service error: {error:?}")
        }
    }
}
#[test]
fn concurrent_public_calls_share_one_real_manager_without_torn_requests_or_lost_cleanup()
{
    mac_system::testing::shared::with_memory_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let busy = AtomicUsize::new(0);
        let mut common = system.memory().new_handle_clear(32768).unwrap();
        let barrier = Barrier::new(8);
        thread::scope(|scope| {
            for worker in 1..=8u8
            {
                let common = &common;
                let system = &system;
                let busy = &busy;
                let barrier = &barrier;
                scope.spawn(move || {
                         barrier.wait();
                         let pattern = vec![worker; 32768];
                         for _ in 0..48
                         {
                             retry(busy, || common.write(0, &pattern));
                             let mut output = vec![0u8; 32768];
                             assert_eq!(retry(busy, || common.read(0, &mut output)), output.len());
                             assert!((1..=8).contains(&output[0]));
                             assert!(output.iter().all(|b| *b == output[0]),
                                     "shared request observed a torn write");
                             let mut own = retry(busy, || {
                                 system.memory().handle_from_bytes(&pattern[..4096])
                             });
                             retry(busy, || own.resize(8192));
                             let mut copy = [0u8; 4096];
                             assert_eq!(retry(busy, || own.read(0, &mut copy)), 4096);
                             assert!(copy.iter().all(|b| *b == worker),
                                     "independent handle aliases another caller");
                             retry(busy, || own.try_dispose());
                         }
                     });
            }
        });
        retry(&busy, || common.try_dispose());
        // Reclaiming almost the entire configured arena proves explicit retries
        // did not lose ownership/cleanup while another caller held the service.
        let mut whole = system.memory().new_handle_clear(1024 * 1024 - 4096).unwrap();
        whole.try_dispose().unwrap();
        assert!(busy.load(Ordering::Relaxed) > 0, "stress did not exercise actual contention");
    });
}
