//! Test-only trusted application context around the real resident File Manager.
//! The public SDK and ABI contain no caller-selected owner field.
pub(super) mod scopes;
use std::cell::Cell;

pub use scopes::{Operation, with_operation};

#[derive(Debug)]
pub struct ModuleReadError(crate::toolbox::files::ModuleReadError);
impl ModuleReadError
{
    pub fn status(&self) -> mac_abi::Status
    {
        self.0.status()
    }

    pub fn format(&self) -> Option<mac_formats::Error>
    {
        match self.0
        {
            crate::toolbox::files::ModuleReadError::Format(cause) => Some(cause),
            _ => None
        }
    }

    pub fn storage(&self) -> Option<mac_abi::Status>
    {
        match self.0
        {
            crate::toolbox::files::ModuleReadError::Storage(status) => Some(status),
            _ => None
        }
    }
}
impl core::fmt::Display for ModuleReadError
{
    fn fmt(&self, output: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        self.0.fmt(output)
    }
}

/// Call native startup's production projected package reader through the real
/// mounted File Manager. Public SDK lookup supplies the file's actual identity;
/// this fixture grants no native module entry or alternate package validator.
pub fn module_resources(
    volume: u32, file: u32)
    -> Result<std::vec::Vec<mac_formats::resource::Resource>, ModuleReadError>
{
    super::SESSION.with(|slot| {
                      let mut slot = slot.borrow_mut();
        let session = slot.as_mut().ok_or(ModuleReadError(crate::toolbox::files::ModuleReadError::Storage(mac_abi::NOT_SUPPORTED)))?;
        session.files.module_resources_checked(volume, file).map_err(ModuleReadError)
                  })
}

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
    use crate::toolbox::handles::KindCode;
    let system = pin_system_owner();
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

fn pin_system_owner() -> &'static crate::toolbox::handles::OwnerRef
{
    use crate::toolbox::handles::OwnerRef;
    static SYSTEM: std::sync::OnceLock<OwnerRef> = std::sync::OnceLock::new();
    SYSTEM.get_or_init(|| OwnerRef::try_new(
        mac_object_table::domain::OwnerIndex::issue(mac_object_table::domain::DomainKind::System).unwrap()).unwrap())
}

/// Hold the same non-cloneable scope a native batch owns while its asynchronous
/// I/O drains. Only completion is supplied by this trusted scheduler fixture;
/// SDK mutation, inspection, invalidation and deferred cleanup are production.
pub fn with_pending_document_pin<R>(volume: u32, file: u32, run: impl FnOnce() -> R)
                                    -> Result<R, mac_abi::Status>
{
    use crate::toolbox::{files::pins::ScopeOwner, handles::KindCode};
    let authority = super::inspection::caller_ledger(current_owner())?;
    let scope = ScopeOwner::new(&authority)?;
    // Correlation label only: this fixture issues no public document IDs.
    const BATCH: u64 = 0;
    super::SESSION.with(|slot| {
                      let mut slot = slot.borrow_mut();
                      let session = slot.as_mut().unwrap();
                      let kinds = [*b"FPIN", *b"FPST", *b"FPOB", *b"FPOS"].map(|code| {
                                                                              session.registry
                                                                             .get(KindCode(code))
                                                                             .unwrap()
                                                                          });
                      session.files.prepare_executable_pins(pin_system_owner().clone(), kinds)?;
                      session.files.pin_document(&authority, BATCH, scope.lease(), volume, file)
                  })?;
    struct Complete(Option<ScopeOwner>);
    impl Drop for Complete
    {
        fn drop(&mut self)
        {
            if let Some(scope) = self.0.take()
            {
                scope.close();
                super::SESSION.with(|slot| {
                                  if let Ok(mut slot) = slot.try_borrow_mut()
                                  {
                                      if let Some(session) = slot.as_mut()
                                      {
                                          session.files.unpin_documents(BATCH);
                                      }
                                  }
                              });
            }
        }
    }
    let _complete = Complete(Some(scope));
    Ok(run())
}
