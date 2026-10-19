//! Test-only trusted application context around the real resident File Manager.
//! The public SDK and ABI contain no caller-selected owner field.
use std::cell::Cell;

std::thread_local! { static OWNER: Cell<u64> = const { Cell::new(1) }; }

pub fn current_owner() -> u64
{
    OWNER.get()
}

/// Select a simulated scheduler identity for synchronous host service calls.
/// Nested scopes restore the previous identity, including during unwind.
pub fn with_owner<R>(owner: u64, run: impl FnOnce() -> R) -> R
{
    struct Restore(u64);
    impl Drop for Restore
    {
        fn drop(&mut self)
        {
            OWNER.set(self.0);
        }
    }
    let _restore = Restore(OWNER.replace(owner));
    run()
}

/// Invoke the same resident cleanup operation that native app exit invokes.
pub fn close_owner(owner: u64) -> usize
{
    super::SESSION.with(|session| {
                      session.borrow_mut()
                             .as_mut()
                             .expect("live file service session")
                             .files
                             .close_all(owner)
                  })
}
