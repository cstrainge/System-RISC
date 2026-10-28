//! Application metadata and activation policy, independent of CPU scheduling.
#![forbid(unsafe_code)]

use mac_abi::{processes::*, *};

use crate::toolbox::handles::{Directory, DirectoryError, DomainKind, OwnerRef};
pub mod menu;
mod presentation;
pub(crate) mod documents;
pub(crate) mod retirement;
pub(crate) mod finder;
pub(crate) fn inspection_request(call: &mut mac_abi::inspection::Call) -> Status
{
    let caller = crate::context::current_thread_context().identity().app.0;
    crate::kernel::with_desktop(|desktop| {
        if matches!(call.operation,
                    mac_abi::inspection::KIND_BEGIN
                    | mac_abi::inspection::KIND
                    | mac_abi::inspection::KIND_ID)
        {
            crate::toolbox::handles::registry(|registry| {
                crate::toolbox::inspection::registered_kinds(registry,
                                                             desktop.processes.get(caller).is_ok(),
                                                             call)
            }).unwrap_or_else(|error| error.status())
        }
        else
        {
            desktop.processes.inspect(caller, call)
        }
    }).unwrap_or_else(|error| error.status())
}
pub(crate) fn memory_inspection_request(call: &mut mac_abi::inspection::memory::Call) -> Status
{
    use crate::toolbox::inspection::Subject;
    let caller = crate::context::current_thread_context().identity().app.0;
    crate::kernel::with_desktop(|desktop| {
        let authority = desktop.processes.entries.get(caller).map(|e| e.value().authority());
        crate::toolbox::inspection::memory::request(authority,
                                                    &crate::machine::paging::inspection::Native,
                                                    call)
    }).unwrap_or_else(|error| error.status())
}

pub struct Process
{
    pub info:         ProcessInfo,
    pub(crate) image: Option<crate::images::Binding>,
    pub stamp:        u64,
    cleanup:          Option<crate::toolbox::handles::cleanup::Checkpoint>,
    account:          crate::machine::scheduler::accounting::ProcessAccount
}

/// A cleanup resource context is minted from one directory entry, never from
/// separately supplied owner/image numbers. The ledger stays pinned throughout
/// the synchronous scope, including for a stopping process without a thread.
pub(crate) struct ResourceBinding
{
    owner: crate::toolbox::handles::OwnerRef,
    image: Option<crate::images::Binding>
}
impl ResourceBinding
{
    pub(crate) fn ledger(&self) -> &crate::toolbox::handles::OwnerRef
    {
        &self.owner
    }

    pub(crate) fn owner(&self) -> u64
    {
        self.owner.owner().incarnation()
    }

    pub(crate) fn image(&self) -> Option<crate::images::Binding>
    {
        self.image
    }
}
impl crate::toolbox::inspection::Subject for Process
{
    fn label(&self) -> &str
    {
        self.info.name()
    }

    fn foreground(&self) -> bool
    {
        self.info.foreground != 0
    }

    fn authority(&self) -> u64
    {
        self.image
            .and_then(|image| {
                crate::kernel::with_image(image, |image| image.application_authority()).flatten()
            })
            .unwrap_or(0)
    }

    fn execution_account(&self) -> Option<mac_kernel_model::account::Snapshot>
    {
        Some(self.account.snapshot())
    }
}
pub struct Manager
{
    entries:           Directory<Process>,
    documents:         documents::storage::Batches,
    document_sequence: u64,
    stamp:             u64
}
impl Manager
{
    pub(crate) fn inspect(&self, caller: u64, call: &mut mac_abi::inspection::Call) -> Status
    {
        crate::drivers::inspect(&self.entries, caller, call)
    }

    pub fn new() -> Self
    {
        Self { entries:           Directory::new(),
               documents:         documents::storage::Batches::new(),
               document_sequence: 0,
               stamp:             0 }
    }

    pub(crate) fn ledger(&self, id: u64) -> Result<crate::toolbox::handles::OwnerRef, Status>
    {
        self.entries.get(id).map(|entry| entry.ledger().clone()).ok_or(NOT_FOUND)
    }

    pub(crate) fn thread_charges(
        &self, id: u64)
        -> Result<crate::machine::scheduler::accounting::ThreadCharges, Status>
    {
        use crate::machine::scheduler::accounting::ThreadCharges;
        let entry = self.entries.get(id).ok_or(NOT_FOUND)?;
        let account = &entry.value().account;
        ThreadCharges::admit(entry.ledger(), account).map_err(|error| {
            serial!("PROCESS_THREAD_ADMISSION_REFUSED: {error}");
            error.status()
        })
    }

    pub(crate) fn resource_binding(&self, id: u64) -> Result<ResourceBinding, Status>
    {
        let entry = self.entries.get(id).ok_or(NOT_FOUND)?;
        Ok(ResourceBinding { owner: entry.ledger().clone(),
                             image: entry.value().image })
    }

    pub fn len(&self) -> usize
    {
        self.iter().count()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Process>
    {
        // Dying/quarantined domains remain inspectable until their death
        // barrier closes, but are no longer running applications.
        self.entries
            .iter()
            .filter(|entry| presentation::running(entry.ledger(), &entry.value().info))
            .map(|entry| entry.value())
    }

    pub(super) fn is_running(&self, id: u64) -> bool
    {
        self.entries
            .get(id)
            .is_some_and(|entry| presentation::running(entry.ledger(), &entry.value().info))
    }

    pub(super) fn hide_others(&mut self, foreground: u64) -> Result<bool, Status>
    {
        let entry = self.entries.get(foreground).ok_or(NOT_FOUND)?;
        presentation::require_foreground(entry.ledger(), &entry.value().info)?;
        let mut changed = false;
        for (ledger, process) in self.entries.entries_mut()
        {
            changed |= presentation::hide_other(ledger, &mut process.info, foreground);
        }
        Ok(changed)
    }

    pub(super) fn stopping_after(&self, previous: u64) -> Option<u64>
    {
        self.entries
            .iter()
            .find(|entry| entry.owner().incarnation() > previous && !entry.ledger().accepting())
            .map(|entry| entry.owner().incarnation())
    }

    pub(super) fn file_stopping(&self, volume: u32, cnid: u32) -> bool
    {
        self.entries.iter().any(|entry| {
                               !entry.ledger().accepting()
                               && entry.value().info.volume == volume
                               && entry.value().info.cnid == cnid
                           })
    }

    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut Process>
    {
        self.entries.values_mut()
    }

    pub(crate) fn bind_image(&mut self, id: u64, image: crate::images::Binding)
                             -> Result<(), Status>
    {
        if image.owner() != id
        {
            return Err(ACCESS_DENIED);
        }
        if !self.ledger(id)?.accepting()
        {
            return Err(SERVICE_BUSY);
        }
        let process = self.get_mut(id)?;
        if process.image.is_some()
        {
            return Err(BAD_ABI);
        }
        process.image = Some(image);
        Ok(())
    }

    pub(crate) fn provision(&mut self, name: &str, volume: u32, cnid: u32) -> Result<u64, Status>
    {
        let mut info = ProcessInfo { volume,
                                     cnid,
                                     ..Default::default() };
        if name.is_empty() || name.len() > info.name.len()
        {
            return Err(BAD_ARGUMENT);
        }
        info.name[..name.len()].copy_from_slice(name.as_bytes());
        info.name_length = name.len() as u32;
        use mac_kernel_model::policy::ConfiguredRange;

        use crate::machine::{memory,
                             scheduler::accounting::{ProcessAccount, account_status}};
        let ledger = OwnerRef::issue(DomainKind::Process).map_err(|error| {
                                                             serial!("PROCESS_DOMAIN_REFUSED: \
                                                                      {error}");
                                                             NO_MEMORY
                                                         })?;
        let id = ledger.owner().incarnation();
        info.id = id;
        let kernel =
            ConfiguredRange::new(0,
                                 memory::PROCESS_KERNEL_BUDGET_BYTES,
                                 memory::PROCESS_KERNEL_BUDGET_BYTES).map_err(|_| BAD_ABI)?;
        let application =
            ConfiguredRange::new(0,
                                 memory::PROCESS_APPLICATION_BUDGET_BYTES,
                                 memory::PROCESS_APPLICATION_BUDGET_BYTES).map_err(|_| BAD_ABI)?;
        let account =
            ProcessAccount::try_new(ledger.clone(), kernel, application).map_err(|error| {
                serial!("PROCESS_ACCOUNT_REFUSED: {error}");
                account_status(&error)
            })?;
        let allocation = account.admit_use().map_err(|error| account_status(&error))?;
        let cleanup = crate::allocator::with_target(crate::allocator::Target::Owner {
            account: allocation, class: mac_kernel_model::policy::MemoryClass::Kernel
        }, || crate::toolbox::handles::cleanup::Checkpoint::try_new()).map_err(|_| NO_MEMORY)?;
        // Only a fully constructed owner/account/checkpoint is published. The
        // directory scopes its retained shared vector independently.
        self.entries
            .insert(ledger,
                    Process { info,
                              image: None,
                              stamp: 0,
                              cleanup: Some(cleanup),
                              account })
            .map_err(|error| {
                serial!("PROCESS_DOMAIN_REFUSED: {error}");
                NO_MEMORY
            })?;
        Ok(id)
    }

    pub fn get(&self, id: u64) -> Result<&Process, Status>
    {
        self.entries.get(id).map(|entry| entry.value()).ok_or(NOT_FOUND)
    }

    pub fn get_mut(&mut self, id: u64) -> Result<&mut Process, Status>
    {
        self.entries.get_mut(id).ok_or(NOT_FOUND)
    }

    pub fn activate(&mut self, id: u64) -> Result<(), Status>
    {
        self.get(id)?;
        if !self.entries.get(id).is_some_and(|entry| entry.ledger().accepting())
        {
            return Err(SERVICE_BUSY);
        }
        self.stamp += 1;
        let stamp = self.stamp;
        for p in self.values_mut()
        {
            p.info.foreground = u32::from(p.info.id == id);
            if p.info.id == id
            {
                p.info.hidden = 0;
                p.stamp = stamp;
            }
        }
        Ok(())
    }

    pub fn previous(&self, excluding: u64) -> u64
    {
        self.iter()
            .filter(|p| p.info.id != excluding && p.info.hidden == 0)
            .max_by_key(|p| p.stamp)
            .map_or(crate::context::finder_app().0, |p| p.info.id)
    }

    pub(super) fn cleanup_checkpoint(
        &self, id: u64)
        -> Result<crate::toolbox::handles::cleanup::Checkpoint, Status>
    {
        self.entries.get(id).ok_or(NOT_FOUND)?.value().cleanup.clone().ok_or(SERVICE_BUSY)
    }

    pub(crate) fn allocation(
        &self, id: u64)
        -> Result<crate::machine::scheduler::accounting::AllocationAuthority, Status>
    {
        self.get(id)?
            .account
            .admit_use()
            .map_err(|error| crate::machine::scheduler::accounting::account_status(&error))
    }

    pub(super) fn cleanup_drained(&self, id: u64) -> Result<bool, Status>
    {
        Ok(self.get(id)?.cleanup.is_none())
    }

    /// Called after cleanup has settled and the saved managers are parked.
    /// The caller then destroys them before checking the account barrier.
    /// Removing this ready checkpoint is irreversible; retries wait on receipts.
    pub(super) fn drain_checkpoint(&mut self, id: u64) -> Result<(), Status>
    {
        if crate::machine::scheduler::has_threads(id)
        {
            return Err(SERVICE_BUSY);
        }
        let ledger = self.ledger(id)?;
        let remaining = ledger.summary();
        if ledger.accepting()
           || remaining.live != 0
           || remaining.pending != 0
           || remaining.quarantined != 0
           || remaining.completion_faults != 0
        {
            return Err(SERVICE_BUSY);
        }
        let process = self.get_mut(id)?;
        if let Some(checkpoint) = &process.cleanup
        {
            checkpoint.ensure_settled().map_err(|_| SERVICE_BUSY)?;
        }
        drop(process.cleanup.take());
        Ok(())
    }

    pub fn remove(&mut self, id: u64) -> Result<(), Status>
    {
        // A retired thread still owns its context and stack until a different
        // stack reclaims it. An empty object ledger alone is insufficient.
        if crate::machine::scheduler::has_threads(id)
        {
            return Err(SERVICE_BUSY);
        }
        if self.get(id)?.cleanup.is_some()
        {
            return Err(SERVICE_BUSY);
        }
        let retirement_error = |error| {
            serial!("PROCESS_DOMAIN_RETIRE_REFUSED: {error}");
            match error
            {
                DirectoryError::NotFound { .. } => NOT_FOUND,
                DirectoryError::Duplicate { .. } => BAD_ARGUMENT,
                DirectoryError::AdmissionOpen { .. }
                | DirectoryError::LiveResources { .. }
                | DirectoryError::PendingResources { .. }
                | DirectoryError::CompletionFaults { .. } => SERVICE_BUSY,
                DirectoryError::Storage { .. } => NO_MEMORY
            }
        };
        // Empty per-domain pools still own accounted bitmap/generation backing.
        // Detach it only after actual cleanup, and release it outside the owner
        // lock before asking that account whether its final receipts are gone.
        self.entries
            .get(id)
            .ok_or(NOT_FOUND)?
            .ledger()
            .drain_stopped()
            .map_err(&retirement_error)?;
        // A cancelled preparation or retained thread reader can outlive its
        // ordinary scheduler ID. The account's original receipts are part of
        // the domain's death barrier, including zero-byte reservations.
        let account = &self.get(id)?.account;
        let was_closing = account.snapshot().closing;
        if !account.close_and_ready()
        {
            if !was_closing
            {
                let remaining = account.snapshot();
                serial!("PROCESS_ACCOUNT_WAIT app={id} kernel_committed={} kernel_reserved={} \
                         application_committed={} application_reserved={} retained_pins={}; \
                         cleanup and stacks are drained, waiting for original allocations or uses",
                        remaining.kernel.committed(),
                        remaining.kernel.reserved(),
                        remaining.application.committed(),
                        remaining.application.reserved(),
                        remaining.retained_pins);
            }
            return Err(SERVICE_BUSY);
        }
        self.entries.remove(id).map(|_| ()).map_err(retirement_error)
    }

    pub fn snapshot(&self, id: u64) -> Result<ProcessInfo, Status>
    {
        let mut info = self.get(id)?.info;
        info.switches = crate::machine::scheduler::switches(id);
        info.image = self.get(id)?.image.map_or(0, crate::images::Binding::id);
        Ok(info)
    }
}
/// Dispatch native and classic process requests without holding desktop state
/// across storage reads, provider callbacks or scheduler waits.
pub(crate) fn request(call: &mut ProcessCall) -> Status
{
    let result = (|| {
        if call.operation != LAUNCH
        {
            return crate::kernel::with_desktop(|d| d.process(call)).map_err(|e| e.status());
        }
        // An app can quit and another launch it before the next timer tick.
        // This trap is already on the other app's stack: finish that real
        // checkpoint before deciding whether reloading the old image is safe.
        crate::machine::scheduler::reclaim_retired();
        retirement::pump();
        if !crate::toolbox::files::application::pending()
           && crate::kernel::with_desktop(|d| d.activate_existing_application(call))
                .map_err(|e| e.status())??
        {
            return Ok(OK);
        }
        let owner = crate::context::current_thread_context().identity().app.0;
        let loaded = crate::toolbox::files::application::read(owner, call.volume, call.cnid)?;
        crate::kernel::with_desktop(|d| d.launch_application(call, *loaded)).map_err(|e| {
                                                                                e.status()
                                                                            })?
                                                                            .map(|()| OK)
    })();
    result.unwrap_or_else(|error| error)
}

impl Default for Manager
{
    fn default() -> Self
    {
        Self::new()
    }
}
