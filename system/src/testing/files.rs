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
    let ledger = super::inspection::caller_ledger(owner).expect("trusted fixture owner");
    let summary = ledger.summary();
    let fork_count = (0..summary.kinds).map(|at| ledger.pool_at(summary.revision, at).unwrap())
                                       .filter(|pool| {
                                           [mac_object_table::domain::KindCode(*b"FFRK"),
                               mac_object_table::domain::KindCode(*b"FDWR"),
                               mac_object_table::domain::KindCode(*b"FSWR"),
                               crate::toolbox::handles::KindCode(*b"FIGR")].contains(&pool.kind)
                                       })
                                       .map(|pool| pool.live)
                                       .sum::<usize>();
    let walked = super::inspection::walk_registered(ledger.owner().incarnation(), false, false);
    assert!(walked.failures.is_empty(), "{}", walked.failures.join("; "));
    fork_count
}

/// One real bounded native rollback step, in place of the guest scheduler's
/// cleanup checkpoint. Tests cannot supply completion or edit the writer state.
pub fn pump_stream_cleanup()
{
    super::SESSION.with(|session| {
                      session.borrow_mut()
                             .as_mut()
                             .expect("live streamed-write fixture session")
                             .files
                             .pump_stream_cleanup();
                  });
}

/// Supply launch's trusted ledger binding to the real executable-pin service.
/// No public trap admits a caller-chosen process owner or target alias ID.
pub fn pin_executable(volume: u32, file: u32) -> Result<(), mac_abi::Status>
{
    use crate::toolbox::handles::{KindCode, OwnerRef};
    static SYSTEM: std::sync::OnceLock<OwnerRef> = std::sync::OnceLock::new();
    let system = SYSTEM.get_or_init(|| OwnerRef::try_new(
        mac_object_table::domain::OwnerIndex::issue(mac_object_table::domain::DomainKind::System).unwrap()).unwrap());
    let owner = super::inspection::caller_ledger(current_owner())?;
    super::SESSION.with(|slot| {
                      let mut slot = slot.borrow_mut();
                      let session = slot.as_mut().unwrap();
                      let kinds = [*b"FPIN", *b"FPST", *b"FPOB", *b"FPOS"].map(|code| {
                                                                              session.registry
                                                                             .get(KindCode(code))
                                                                             .unwrap()
                                                                          });
                      session.files.prepare_executable_pins(system.clone(), kinds)?;
                      session.files.pin_executable(&owner, volume, file)
                  })
}
