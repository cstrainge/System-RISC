//! A real APPL with ordinary Rust startup. No custom entry, allocator, panic
//! handler or host service substitute. Execute only in an owned private guest.
use std::{cell::RefCell,
          error::Error,
          hint::black_box,
          sync::{Condvar, Mutex, OnceLock, TryLockError},
          time::{Duration, Instant, SystemTime}};

use mac_toolbox::{Rect, System, windows::WindowSpec};

struct Witness(String);
impl Drop for Witness
{
    fn drop(&mut self)
    {
        // Keep the destructor independent of stdio and already-destroyed TLS.
        // The String still has to deallocate through the real application heap.
        if let Ok(system) = System::new()
        {
            let _ = system.log("STD_PROBE_TLS_DTOR\n");
        }
    }
}
thread_local! {
    static VALUE: RefCell<Option<Witness>> = const { RefCell::new(None) };
}

fn main() -> Result<(), Box<dyn Error>>
{
    let system = System::new()?;
    let repeated = System::new()?;
    let process = system.processes().current()?;
    assert_eq!(process.id, repeated.processes().current()?.id, "System acquisition changed caller");
    system.log(&format!("STD_PROBE_BEGIN app={} thread={}\n", process.id.0, process.thread.0))?;

    let mut bytes = vec![0x5au8; 8192];
    bytes.try_reserve_exact(8192)?;
    bytes.resize(16384, 0xa5);
    assert!(black_box(&bytes[..8192]).iter().all(|&byte| byte == 0x5a),
            "Vec growth lost old contents");
    assert!(black_box(&bytes[8192..]).iter().all(|&byte| byte == 0xa5),
            "Vec growth lost new contents");
    let text = format!("std String/Box/Vec: αβ 🦀 {}", bytes.len());
    let boxed = Box::new(text.clone());
    assert_eq!(*boxed, text);
    VALUE.with(|slot| *slot.borrow_mut() = Some(Witness(text)));
    VALUE.with(|slot| assert!(slot.borrow().as_ref().unwrap().0.contains("16384")));
    system.log("STD_PROBE_COLLECTIONS_TLS_OBSERVED\n")?;

    let before = Instant::now();
    let wall = SystemTime::now();
    assert!(wall > SystemTime::UNIX_EPOCH, "wall clock has no contemporary epoch");
    let old = SystemTime::UNIX_EPOCH.checked_sub(Duration::from_nanos(1)).unwrap();
    assert_eq!(SystemTime::UNIX_EPOCH.duration_since(old).unwrap(), Duration::from_nanos(1));
    assert_eq!(old.checked_add(Duration::from_nanos(1)), Some(SystemTime::UNIX_EPOCH));
    assert!(SystemTime::UNIX_EPOCH.checked_add(Duration::MAX).is_none());
    assert!(SystemTime::UNIX_EPOCH.checked_sub(Duration::MAX).is_none());
    assert!(Instant::now() >= before, "native monotonic clock went backwards");
    system.log("STD_PROBE_CLOCKS_OBSERVED\n")?;

    let mutex = Mutex::new(7usize);
    let held = mutex.lock().unwrap();
    assert!(matches!(mutex.try_lock(), Err(TryLockError::WouldBlock)));
    drop(held);
    let condition = Condvar::new();
    let (held, timeout) = condition.wait_timeout_while(mutex.lock().unwrap(),
                                                       Duration::from_millis(10),
                                                       |value| *value == 7)
                                   .unwrap();
    assert!(timeout.timed_out(), "condition timeout did not reach its deadline");
    drop(held);
    let once = OnceLock::new();
    assert_eq!(*once.get_or_init(|| 13), 13);
    assert_eq!(*once.get_or_init(|| panic!("OnceLock initialized twice")), 13);
    system.log("STD_PROBE_SYNC_TIMEOUT_OBSERVED\n")?;

    let window =
        system.windows().create(WindowSpec::new(Rect::new(60, 80, 290, 440), "Std Probe"))?;
    system.close_window(window)?;
    drop(boxed);
    drop(bytes);
    system.log("STD_PROBE_MAIN_RETURN\n")?;
    // The platform, not author code, runs VALUE's destructor after main returns.
    Ok(())
}
