//! Native provider requests through the public System facade. No statistical
//! conclusion is drawn from source bytes and no source bytes are logged.
use std::{error::Error, thread};

use mac_toolbox::{System,
                  abi::{self, entropy as wire}};

#[cfg(any(all(feature = "entropy-unavailable", feature = "entropy-short"),
          all(feature = "entropy-unavailable", feature = "entropy-cancel"),
          all(feature = "entropy-short", feature = "entropy-cancel")))]
compile_error!("select one entropy environment scenario");

fn malformed(system: &System) -> Result<(), Box<dyn Error>>
{
    let mut output = [0xa5u8; 32];
    for index in 0..8
    {
        let mut call = wire::Call::default();
        match index
        {
            0 => call.operation = u32::MAX,
            1 => call.flags = 1,
            2 => call.reserved[0] = 1,
            3 => call.reserved2 = 1,
            4 =>
            {
                call.output = output.as_mut_ptr();
                call.capacity = output.len() as u32;
            }
            5 => call.operation = wire::FILL,
            6 =>
            {
                call.operation = wire::FILL;
                call.capacity = 1;
            }
            7 => call.capacity = wire::MAX_CAPACITY as u32 + 1,
            _ => unreachable!()
        }
        let mut status = core::ptr::from_mut(&mut call).addr();
        // SAFETY: a live exclusive request and, where supplied, valid disjoint
        // output reach the actual trap. Null/oversize fields deliberately test
        // rejection before slice creation; no invalid Rust reference is made.
        unsafe {
            core::arch::asm!("ecall", inlateout("a0") status,
                in("a1") core::mem::size_of::<wire::Call>(),
                in("a7") abi::traps::ENTROPY, options(nostack));
        }
        assert_eq!(status as isize, abi::BAD_ARGUMENT as isize);
        assert_eq!(call.written, 0);
        let detail = core::str::from_utf8(&call.detail[..call.detail_length as usize])?;
        assert!(!detail.is_empty());
        assert!(output.iter().all(|&byte| byte == 0xa5));
    }
    system.log("STD_PROBE_ENTROPY_MALFORMED refusals=8 output_unchanged=true\n")?;
    Ok(())
}

pub(super) fn run(system: &System) -> Result<(), Box<dyn Error>>
{
    malformed(system)?;
    system.entropy().fill(&mut [])?;
    assert_eq!(system.entropy().fill_some(&mut [])?, 0);
    #[cfg(feature = "entropy-unavailable")]
    {
        for attempt in 0..2
        {
            let failure = system.entropy().ready().unwrap_err();
            assert_eq!(failure.cause(), mac_toolbox::Error::NotFound);
            system.log(&format!("STD_PROBE_ENTROPY_UNAVAILABLE attempt={attempt} {failure}\n"))?;
            let mut untouched = [0xcdu8; 64];
            let failure = system.entropy().fill(&mut untouched).unwrap_err();
            assert_eq!(failure.initialized(), 0);
            assert_eq!(failure.failure().cause(), mac_toolbox::Error::NotFound);
            assert!(untouched.iter().all(|&byte| byte == 0xcd));
        }
        system.log("STD_PROBE_ENTROPY_NEGATIVE retry=true output_unchanged=true\n")?;
        return Ok(());
    }
    #[cfg(not(feature = "entropy-unavailable"))]
    {
        system.entropy().ready()?;
        system.log("STD_PROBE_ENTROPY_READY\n")?;
        #[cfg(feature = "entropy-cancel")]
        {
            use std::{sync::{Arc, Condvar, Mutex},
                      time::Duration};
            let entered = Arc::new((Mutex::new(false), Condvar::new()));
            let child_entered = entered.clone();
            let child = thread::spawn(move || {
                let system = System::new().unwrap();
                let id = system.client().current_native_thread().unwrap();
                system.log(&format!("STD_PROBE_ENTROPY_PENDING_CHILD thread={}\n", id.0)).unwrap();
                let (lock, changed) = &*child_entered;
                *lock.lock().unwrap() = true;
                changed.notify_one();
                let mut output = [0u8; 4096];
                system.entropy().fill(&mut output).unwrap();
                panic!("throttled entropy fill completed before intended process cancellation");
            });
            let (lock, changed) = &*entered;
            let (guard, timeout) = changed.wait_timeout_while(lock.lock().unwrap(),
                                                              Duration::from_secs(5),
                                                              |value| !*value)
                                          .unwrap();
            assert!(!timeout.timed_out());
            drop(guard);
            // This guest delay allows a pending DMA observation. It is not an
            // assertion of an interleaving; the external observer must capture
            // the real outstanding descriptor before claiming cancellation.
            thread::sleep(Duration::from_secs(3));
            drop(child);
            system.log("STD_PROBE_ENTROPY_CANCEL_EXIT\n")?;
            std::process::exit(-31);
        }
        #[cfg(not(feature = "entropy-cancel"))]
        {
            let mut guarded = [0xa5u8; 2064];
            let count = system.entropy().fill_some(&mut guarded[8..2056])?;
            assert!((1..=2048).contains(&count));
            assert!(guarded[..8].iter()
                                .chain(guarded[8 + count..].iter())
                                .all(|&byte| byte == 0xa5));
            #[cfg(feature = "entropy-short")]
            assert!(count < 2048, "configured rate limit did not yield a real short completion");
            system.log(&format!("STD_PROBE_ENTROPY_SOME count={count} guards=true\n"))?;
            #[cfg(feature = "entropy-short")]
            let length = 512;
            #[cfg(not(feature = "entropy-short"))]
            let length = wire::MAX_CAPACITY + 17;
            let mut whole = vec![0u8; length];
            system.entropy().fill(&mut whole)?;
            std::hint::black_box(&whole);
            system.log(&format!("STD_PROBE_ENTROPY_FULL bytes={length}\n"))?;
            let mut children = Vec::new();
            for _ in 0..4
            {
                children.push(thread::spawn(|| {
                                  let system = System::new().unwrap();
                                  let id = system.client().current_native_thread().unwrap().0;
                                  for _ in 0..8
                                  {
                                      let mut output = [0u8; 31];
                                      system.entropy().fill(&mut output).unwrap();
                                      std::hint::black_box(output);
                                  }
                                  id
                              }));
            }
            let identities: Vec<_> =
                children.into_iter().map(|child| child.join().unwrap()).collect();
            let mut unique = identities.clone();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!(unique.len(), 4);
            system.entropy().ready()?;
            system.entropy().fill(&mut [0u8; 16])?;
            system.log(&format!("STD_PROBE_ENTROPY_THREADS ids={identities:?} fills=32 \
                                 reuse=true\n"))?;
            Ok(())
        }
    }
}
