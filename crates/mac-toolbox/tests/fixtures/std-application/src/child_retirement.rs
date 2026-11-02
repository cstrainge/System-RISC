//! Ordinary std child entry retained across native process termination.
use std::{error::Error,
          sync::{Arc, Condvar, Mutex},
          thread,
          time::Duration};

use mac_toolbox::System;

struct ChildTls;
impl Drop for ChildTls
{
    fn drop(&mut self)
    {
        if let Ok(system) = System::new()
        {
            let _ = system.log("STD_PROBE_CHILD_TLS_DTOR\n");
        }
    }
}
thread_local! {
    static CHILD_TLS: ChildTls = const { ChildTls };
}

pub(super) fn run(system: &System) -> Result<(), Box<dyn Error>>
{
    let ready = Arc::new((Mutex::new(false), Condvar::new()));
    let child_ready = ready.clone();
    let child = thread::Builder::new().name("std retirement child".into()).spawn(move || {
        let system = System::new().unwrap();
        let id = system.client().current_native_thread().unwrap();
        CHILD_TLS.with(|value| {
                     std::hint::black_box(value);
                 });
        let allocation = vec![0xadu8; 32768];
        system.log(&format!("STD_PROBE_LIVE_CHILD thread={} bytes={}\n", id.0, allocation.len()))
              .unwrap();
        let (lock, changed) = &*child_ready;
        *lock.lock().unwrap() = true;
        changed.notify_one();
        #[cfg(feature = "child-panic")]
        {
            // Give the parent a chance to enter JOIN. This interval does not
            // establish an exact scheduler interleaving or a latency bound.
            thread::sleep(Duration::from_millis(30));
            panic!("STD_PROBE_CHILD_PANIC: UTF-8 αβ 🦀; default System hook");
        }
        #[cfg(feature = "live-child")]
        loop
        {
            // A spurious wake cannot finish the child. Its original heap/TLS/
            // stack resources must be reclaimed by actual Process shutdown.
            std::hint::black_box(&allocation);
            thread::park();
        }
    })?;
    let (lock, changed) = &*ready;
    let (guard, timeout) =
        changed.wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(5), |ready| !*ready)
               .unwrap();
    assert!(!timeout.timed_out(), "child did not enter before retirement scenario");
    drop(guard);
    #[cfg(feature = "child-panic")]
    {
        system.log("STD_PROBE_PARENT_JOINING_PANIC_CHILD\n")?;
        child.join().unwrap();
        panic!("STD_PROBE_CHILD_PANIC unexpectedly returned");
    }
    #[cfg(feature = "live-child")]
    {
        drop(child);
        system.log("STD_PROBE_DETACHED_CHILD_RETAINED\n")?;
        Ok(())
    }
}
