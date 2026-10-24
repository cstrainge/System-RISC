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

/// Hold a real probe lease while public SDK calls invalidate its watch. Only
/// transport completion is supplied by this trusted fixture; the production
/// store, guard and ledger determine lifetime and slot reuse. Nested service
/// scopes restore this session before the private RAII ACK runs on unwind.
pub fn with_pending_watch_probe<R>(handle: u32, run: impl FnOnce() -> R)
                                   -> Result<R, mac_abi::Status>
{
    let owner = super::inspection::caller_ledger(current_owner())?;
    let lease = super::SESSION.with(|slot| {
                                  slot.borrow_mut()
                                      .as_mut()
                                      .unwrap()
                                      .files
                                      .begin_fixture_probe(&owner, handle)
                              })?;
    struct Complete(crate::toolbox::files::watch::ProbeLease);
    impl Drop for Complete
    {
        fn drop(&mut self)
        {
            super::SESSION.with(|slot| {
                              slot.borrow_mut()
                                  .as_mut()
                                  .unwrap()
                                  .files
                                  .finish_fixture_probe(&self.0)
                                  .expect("trusted provider fixture's unique acknowledgement");
                          });
        }
    }
    let _complete = Complete(lease);
    Ok(run())
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
