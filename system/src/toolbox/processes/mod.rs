//! Application metadata and activation policy, independent of CPU scheduling.
#![forbid(unsafe_code)]

use mac_abi::{processes::*, *};

use crate::toolbox::handles::{Directory, DirectoryError, DomainKind, OwnerRef};
pub mod menu;
mod presentation;
pub(crate) mod documents;
pub(crate) mod retirement;
pub(crate) mod finder;
pub(crate) mod termination;
#[cfg(target_arch = "riscv64")]
pub(super) mod execution;
pub(crate) mod launch;
pub(crate) mod startup;
pub(crate) fn inspection_request(call: &mut mac_abi::inspection::Call) -> Status
{
    let caller = crate::context::current_thread_context().identity().app.0;
    crate::kernel::with_toolbox(|toolbox| {
        if matches!(call.operation,
                    mac_abi::inspection::KIND_BEGIN
                    | mac_abi::inspection::KIND
                    | mac_abi::inspection::KIND_ID)
        {
            crate::toolbox::handles::registry(|registry| {
                crate::toolbox::inspection::registered_kinds(registry,
                                                             toolbox.processes.get(caller).is_ok(),
                                                             call)
            }).unwrap_or_else(|error| error.status())
        }
        else
        {
            toolbox.processes.inspect(caller, call)
        }
    }).unwrap_or_else(|error| error.status())
}
pub(crate) fn memory_inspection_request(call: &mut mac_abi::inspection::memory::Call) -> Status
{
    use crate::toolbox::inspection::Subject;
    let caller = crate::context::current_thread_context().identity().app.0;
    crate::kernel::with_toolbox(|toolbox| {
        let authority = toolbox.processes.entries.get(caller).map(|e| e.value().authority());
        crate::toolbox::inspection::memory::request(authority,
                                                    &crate::machine::paging::inspection::Native,
                                                    call)
    }).unwrap_or_else(|error| error.status())
}

pub(crate) fn cpu_inspection_request(call: &mut mac_abi::inspection::cpus::Call) -> Status
{
    let caller = crate::context::current_thread_context().identity().app.0;
    crate::kernel::with_toolbox(|toolbox| {
        crate::toolbox::inspection::cpus::request(toolbox.processes
                                                         .entries
                                                         .get(caller)
                                                         .is_some_and(|entry| {
                                                             entry.ledger().accepting()
                                                         }),
                                                  call)
    }).unwrap_or_else(|error| error.status())
}

pub struct Process
{
    pub info:                        ProcessInfo,
    pub(crate) image:                Option<crate::images::Binding>,
    pub(super) streams:              [u64; 3],
    argv:                            Option<Arguments>,
    pub(crate) opened_for_documents: bool,
    pub(super) console_session:      Option<(u64, u64)>,
    parent:                          Option<u64>,
    children:                        alloc::vec::Vec<ChildReceipt>,
    // Original executable context, captured at its real read. Not authority.
    original_catalog:                Option<mac_abi::files::CatalogInfo>,
    heap:                            Option<crate::toolbox::application_heap::RootBinding>,
    tls:                             Option<crate::toolbox::native_threads::RootBinding>,
    pub stamp:                       u64,
    cleanup:                         Option<crate::toolbox::handles::cleanup::Checkpoint>,
    termination:                     termination::State,
    account:                         crate::machine::scheduler::accounting::ProcessAccount
}

struct Arguments
{
    bytes:  alloc::vec::Vec<u8>,
    length: usize,
    count:  u32
}

struct ChildReceipt
{
    id:      u64,
    outcome: Option<termination::Outcome>
}

impl Process
{
    pub(super) fn console(&self) -> bool
    {
        self.original_catalog.is_some_and(|catalog| catalog.file_type == *b"CLAP")
    }
}

/// Complete authentic Process backing before directory publication. No ID is
/// reconstructed from a label; this owns the one issued ledger and account.
/// It contains no registered object, executable or running thread.
pub(crate) struct PreparedProcess
{
    ledger: OwnerRef,
    value:  Process
}
impl PreparedProcess
{
    pub(crate) fn owner(&self) -> &OwnerRef
    {
        &self.ledger
    }

    pub(crate) fn allocation(
        &self)
        -> Result<crate::machine::scheduler::accounting::AllocationAuthority, Status>
    {
        self.value
            .account
            .admit_use()
            .map_err(|cause| crate::machine::scheduler::accounting::account_status(&cause))
    }

    /// Set the original read's metadata before canonical recipient publication.
    /// The genuine prepared owner/account are unchanged; this creates no lease.
    pub(crate) fn bind_launch_catalog(&mut self, catalog: mac_abi::files::CatalogInfo)
                                      -> Result<(), Status>
    {
        if let Some(original) = self.value.original_catalog
        {
            // A capacity-publication retry keeps the SAME original snapshot;
            // it never refreshes metadata from a later filesystem lookup.
            return if original.volume == catalog.volume
                      && original.id == catalog.id
                      && original.parent == catalog.parent
            {
                Ok(())
            }
            else
            {
                Err(BAD_ABI)
            };
        }
        if (self.value.info.volume != 0 && self.value.info.volume != catalog.volume)
           || (self.value.info.cnid != 0 && self.value.info.cnid != catalog.id)
        {
            return Err(BAD_ABI);
        }
        self.value.info.volume = catalog.volume;
        self.value.info.cnid = catalog.id;
        self.value.original_catalog = Some(catalog);
        Ok(())
    }

    pub(crate) fn prepare(name: &str, volume: u32, cnid: u32) -> Result<Self, Status>
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
        Ok(Self { ledger,
                  value: Process { info,
                                   image: None,
                                   original_catalog: None,
                                   streams: [0; 3],
                                   argv: None,
                                   opened_for_documents: false,
                                   console_session: None,
                                   parent: None,
                                   children: alloc::vec::Vec::new(),
                                   heap: None,
                                   tls: None,
                                   stamp: 0,
                                   cleanup: Some(cleanup),
                                   termination: Default::default(),
                                   account } })
    }
}

/// Owns one genuine, directory-published provisional process across preparation
/// outside Desktop. Its account comes from that same entry, never from IDs.
pub(crate) struct LaunchAdmission
{
    owner:            crate::toolbox::handles::OwnerRef,
    allocation:       crate::machine::scheduler::accounting::AllocationAuthority,
    active:           bool,
    original_catalog: mac_abi::files::CatalogInfo
}
impl LaunchAdmission
{
    pub(crate) fn original_catalog(&self) -> mac_abi::files::CatalogInfo
    {
        self.original_catalog
    }

    pub(crate) fn owner(&self) -> &crate::toolbox::handles::OwnerRef
    {
        &self.owner
    }

    pub(crate) fn id(&self) -> u64
    {
        self.owner.owner().incarnation()
    }

    pub(crate) fn allocation(&self) -> crate::machine::scheduler::accounting::AllocationAuthority
    {
        self.allocation.clone()
    }

    pub(crate) fn started(&mut self)
    {
        self.active = false;
    }

    fn stop(&mut self)
    {
        if self.active
        {
            use mac_object_table::shared::InvocationGate as _;
            crate::toolbox::handles::InvocationGate::run(|| {
                self.owner.stop_admission();
                crate::machine::scheduler::retire_owner(self.id());
            });
            self.active = false;
            retirement::resources_released();
        }
    }
}
impl Drop for LaunchAdmission
{
    fn drop(&mut self)
    {
        self.stop();
    }
}

/// Publish the real executable alias under the prospective process account.
/// Startup and replacement call this once after READY/directory completion,
/// outside Desktop, before publishing any author thread. Catalog IDs select
/// the original read; only the captured owner supplies authority.
fn pin_launch_executable(owner: &OwnerRef,
                         allocation: crate::machine::scheduler::accounting::AllocationAuthority,
                         original: mac_abi::files::CatalogInfo)
                         -> Result<(), Status>
{
    crate::allocator::with_target(crate::allocator::Target::Owner {
        account: allocation,
        class: mac_kernel_model::policy::MemoryClass::Kernel
    }, || crate::kernel::with_files(|files| {
        files.pin_executable(owner, original.volume, original.id)
    })).map_err(crate::state::AccessError::status).and_then(|result| result)
}

/// A cleanup resource context is minted from one directory entry, never from
/// separately supplied owner/image numbers. The ledger stays pinned throughout
/// the synchronous scope, including for a stopping process without a thread.
pub(crate) struct ResourceBinding
{
    owner: crate::toolbox::handles::OwnerRef,
    image: Option<crate::images::Binding>
}
#[derive(Clone, Copy, Debug)]
pub(crate) enum RetirementCheckError
{
    AdmissionOpen,
    BindingChanged
}
impl core::fmt::Display for RetirementCheckError
{
    fn fmt(&self, out: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        out.write_str(match self
                      {
                          Self::AdmissionOpen =>
                          {
                              "old Finder admission is still open after handoff; full retirement \
                               is not established"
                          }
                          Self::BindingChanged =>
                          {
                              "old Finder directory binding differs from its retained owner/image; \
                               full retirement cannot be authorized"
                          }
                      })
    }
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

    pub(crate) fn retain(&self) -> Self
    {
        Self { owner: self.owner.clone(),
               image: self.image }
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

    /// Retain only the original read's context metadata. Actual directory and
    /// mount authority must still be admitted by Files using this process owner.
    pub(crate) fn bind_launch_catalog(&mut self, id: u64, catalog: mac_abi::files::CatalogInfo)
                                      -> Result<(), Status>
    {
        let process = self.get_mut(id)?;
        if process.info.volume != catalog.volume
           || process.info.cnid != catalog.id
           || process.original_catalog.is_some()
           || process.info.thread != 0
        {
            return Err(BAD_ABI);
        }
        process.original_catalog = Some(catalog);
        Ok(())
    }

    pub(crate) fn original_catalog(&self, id: u64) -> Result<mac_abi::files::CatalogInfo, Status>
    {
        self.get(id)?.original_catalog.ok_or(BAD_ABI)
    }

    pub(crate) fn launch_admission(&self, id: u64) -> Result<LaunchAdmission, Status>
    {
        let entry = self.entries.get(id).ok_or(NOT_FOUND)?;
        if !entry.ledger().accepting()
           || entry.value().info.thread != 0
           || entry.value().heap.is_some()
        {
            return Err(SERVICE_BUSY);
        }
        Ok(LaunchAdmission { owner: entry.ledger().clone(),
                             allocation: entry.value().account.admit_use().map_err(|error| {
                                 crate::machine::scheduler::accounting::account_status(&error)
                             })?, active: true,
                             original_catalog: entry.value().original_catalog.ok_or(BAD_ABI)? })
    }

    pub(super) fn file_starting(&self, volume: u32, cnid: u32) -> bool
    {
        self.entries.iter().any(|entry| {
                               entry.ledger().accepting()
                               && entry.value().info.thread == 0
                               && entry.value().info.volume == volume
                               && entry.value().info.cnid == cnid
                           })
    }

    /// Checked attachment cannot substitute a different process incarnation.
    /// The published heap remains in the owner's walk even if this refuses.
    pub(crate) fn bind_heap(&mut self, owner: &OwnerRef,
                            heap: crate::toolbox::application_heap::RootBinding)
                            -> Result<(), Status>
    {
        if heap.owner() != owner.owner()
        {
            return Err(ACCESS_DENIED);
        }
        let entry = self.entries.get(owner.owner().incarnation()).ok_or(NOT_FOUND)?;
        if entry.ledger().owner() != owner.owner()
        {
            return Err(ACCESS_DENIED);
        }
        if !entry.ledger().accepting()
        {
            return Err(SERVICE_BUSY);
        }
        if entry.value().heap.is_some()
        {
            return Err(BAD_ABI);
        }
        self.get_mut(owner.owner().incarnation())?.heap = Some(heap);
        Ok(())
    }

    pub(crate) fn heap(&self, id: u64)
                       -> Result<crate::toolbox::application_heap::RootBinding, Status>
    {
        let entry = self.entries.get(id).ok_or(NOT_FOUND)?;
        if !entry.ledger().accepting()
        {
            return Err(SERVICE_BUSY);
        }
        let heap = entry.value().heap.as_ref().ok_or(BAD_ABI)?;
        if heap.owner() != entry.ledger().owner()
        {
            return Err(ACCESS_DENIED);
        }
        Ok(heap.clone())
    }

    pub(crate) fn bind_tls(&mut self, owner: &OwnerRef,
                           tls: crate::toolbox::native_threads::RootBinding)
                           -> Result<(), Status>
    {
        let entry = self.entries.get(owner.owner().incarnation()).ok_or(NOT_FOUND)?;
        if entry.ledger().owner() != owner.owner() || tls.owner() != owner.owner()
        {
            return Err(ACCESS_DENIED);
        }
        if !entry.ledger().accepting()
        {
            return Err(SERVICE_BUSY);
        }
        if entry.value().tls.is_some() || entry.value().info.thread != 0
        {
            return Err(BAD_ABI);
        }
        self.get_mut(owner.owner().incarnation())?.tls = Some(tls);
        Ok(())
    }

    pub(crate) fn tls(&self, id: u64)
                      -> Result<crate::toolbox::native_threads::RootBinding, Status>
    {
        let entry = self.entries.get(id).ok_or(NOT_FOUND)?;
        if !entry.ledger().accepting()
        {
            return Err(SERVICE_BUSY);
        }
        let tls = entry.value().tls.as_ref().ok_or(BAD_ABI)?;
        if tls.owner() != entry.ledger().owner()
        {
            return Err(ACCESS_DENIED);
        }
        Ok(tls.clone())
    }

    pub(crate) fn plan_provision_capacity(
        &self)
        -> Result<super::handles::DirectoryCapacityPlan<Process>,
                  super::handles::DirectoryCapacityError>
    {
        self.entries.plan_insert_capacity()
    }

    pub(crate) fn install_provision_capacity(
        &mut self, prepared: &mut super::handles::PreparedDirectoryCapacity<Process>)
        -> Result<(), super::handles::DirectoryCapacityError>
    {
        self.entries.install_insert_capacity(prepared)
    }

    pub(crate) fn provision(&mut self, name: &str, volume: u32, cnid: u32) -> Result<u64, Status>
    {
        let prepared = PreparedProcess::prepare(name, volume, cnid)?;
        let id = prepared.ledger.owner().incarnation();
        self.entries.insert(prepared.ledger, prepared.value).map_err(|cause| {
            serial!("PROCESS_DOMAIN_REFUSED: {cause}");
            NO_MEMORY
        })?;
        Ok(id)
    }

    /// No allocation or rejected-payload disposal in this publication turn.
    pub(crate) fn provision_prepared(&mut self, prepared: PreparedProcess)
                                     -> Result<u64, (Status, PreparedProcess)>
    {
        match self.entries.insert_prepared(prepared.ledger, prepared.value)
        {
            Ok(owner) => Ok(owner.incarnation()),
            Err(failure) =>
            {
                let status = match failure.cause
                {
                    DirectoryError::Storage { .. } => SERVICE_BUSY,
                    DirectoryError::Duplicate { .. }
                    | DirectoryError::NotFound { .. }
                    | DirectoryError::LiveResources { .. }
                    | DirectoryError::PendingResources { .. }
                    | DirectoryError::CompletionFaults { .. }
                    | DirectoryError::AdmissionOpen { .. } => BAD_ABI
                };
                Err((status,
                     PreparedProcess { ledger: failure.ledger,
                                       value:  failure.value }))
            }
        }
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
        if self.get(id)?.console()
        {
            return Err(ACCESS_DENIED);
        }
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
            .filter(|p| !p.console() && p.info.id != excluding && p.info.hidden == 0)
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
        drop(core::mem::take(&mut process.children));
        Ok(())
    }

    /// The retained genuine binding is correlation after handoff, not a Read
    /// or account pin. Only actual directory absence establishes full removal;
    /// empty THRD/image tables alone cannot establish that barrier.
    pub(crate) fn retired_binding(&self, old: &ResourceBinding)
                                  -> Result<bool, RetirementCheckError>
    {
        if old.ledger().accepting()
        {
            return Err(RetirementCheckError::AdmissionOpen);
        }
        let Some(process) = self.entries.get(old.owner())
        else
        {
            return Ok(true);
        };
        if process.ledger().owner() != old.ledger().owner()
           || process.value().image.map(crate::images::Binding::id)
              != old.image().map(crate::images::Binding::id)
        {
            return Err(RetirementCheckError::BindingChanged);
        }
        Ok(false)
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
        // The startup arguments are charged to this process. With admission
        // closed and its threads gone, release them before closing the account.
        drop(self.get_mut(id)?.argv.take());
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

    pub(super) fn retain_child(&mut self, parent: u64, child: u64) -> Result<(), Status>
    {
        if parent == child || !self.ledger(parent)?.accepting() || !self.get(child)?.console()
        {
            return Err(ACCESS_DENIED);
        }
        let account = self.allocation(parent)?;
        let children = &mut self.get_mut(parent)?.children;
        crate::allocator::with_target(crate::allocator::Target::Owner {
            account,
            class: mac_kernel_model::policy::MemoryClass::Kernel
        }, || children.try_reserve(1)).map_err(|_| NO_MEMORY)?;
        children.push(ChildReceipt { id:      child,
                                     outcome: None });
        self.get_mut(child)?.parent = Some(parent);
        Ok(())
    }

    pub(super) fn forget_child(&mut self, parent: u64, child: u64) -> Result<(), Status>
    {
        let children = &mut self.get_mut(parent)?.children;
        let at = children.iter().position(|receipt| receipt.id == child).ok_or(NOT_FOUND)?;
        children.swap_remove(at);
        crate::machine::scheduler::wake_child(child);
        if children.is_empty()
        {
            drop(core::mem::take(children));
        }
        Ok(())
    }

    pub(super) fn child_status(&self, parent: u64, call: &mut ProcessCall) -> Result<(), Status>
    {
        use termination::Outcome;
        let receipt = self.get(parent)?
                          .children
                          .iter()
                          .find(|receipt| receipt.id == call.id)
                          .ok_or(NOT_FOUND)?;
        (call.index, call.exit_code) = match receipt.outcome
        {
            None => (CHILD_RUNNING, 0),
            Some(Outcome::Normal(code)) => (CHILD_EXITED, code),
            Some(Outcome::Abort(code)) => (CHILD_ABORTED, code),
            Some(Outcome::Interrupted) => (CHILD_INTERRUPTED, 0),
            Some(Outcome::Legacy | Outcome::ShellReplacement) => (CHILD_ABORTED, 1)
        };
        Ok(())
    }

    pub fn snapshot(&self, id: u64) -> Result<ProcessInfo, Status>
    {
        let mut info = self.get(id)?.info;
        info.switches = crate::machine::scheduler::switches(id);
        info.image = self.get(id)?.image.map_or(0, crate::images::Binding::id);
        Ok(info)
    }
}
/// Acknowledgement belongs to the actual current Finder main, never a wire ID.
/// The moved future-default capability leaves every critical turn before Drop.
fn finder_desktop_ready() -> Result<(), Status>
{
    use mac_object_table::shared::InvocationGate as _;

    let released = crate::context::with_application_executor(|executor| {
                       crate::kernel::with_toolbox(|toolbox| {
                           crate::toolbox::handles::InvocationGate::run(|| {
                               let owner = executor.owner().owner();
                               let id = owner.incarnation();
                               if id != crate::context::finder_app().0
                               {
                                   return Err(ACCESS_DENIED);
                               }
                               let entry = toolbox.processes.entries.get(id).ok_or(NOT_FOUND)?;
                               let process = entry.value();
                               if entry.ledger().owner() != owner
                                  || process.info.id != id
                                  || process.image != Some(executor.image())
                                  || process.account.owner() != owner
                                  || !process.account.owns_use(executor.allocation())
                               {
                                   return Err(ACCESS_DENIED);
                               }
                               if !entry.ledger().accepting()
                                  || process.info.thread == 0
                                  || process.termination.record().is_some()
                                  || process.account.snapshot().closing
                               {
                                   return Err(SERVICE_BUSY);
                               }
                               if process.info.thread != executor.thread()
                               {
                                   return Err(WRONG_THREAD);
                               }
                               // All fallible validation precedes this pointer transition.
                               // Scoped service permits and original receipts are untouched.
                               Ok(executor.take_recovery_default_after_desktop_ready())
                           })
                       }).map_err(|cause| cause.status())?
                   }).map_err(|cause| cause.status())??;
    drop(released);
    Ok(())
}

/// Wait and publication share the Desktop guard. The native trap retains the
/// original request across suspension and replays it after notification.
pub(crate) fn wait_child(call: &mut ChildWaitCall, suspension: Result<(), Status>)
                         -> crate::machine::scheduler::service::RequestResult
{
    use crate::machine::scheduler::service::{RequestAction, RequestResult};
    if call.child == 0 || call.reserved != [0; 2]
    {
        return RequestResult::done(BAD_ARGUMENT);
    }
    let caller = crate::context::current_thread_context().identity().app.0;
    let result = crate::kernel::with_toolbox(|toolbox| {
        let mut query = ProcessCall { id: call.child,
                                      ..Default::default() };
        toolbox.processes.child_status(caller, &mut query)?;
        if query.index == CHILD_RUNNING
        {
            crate::machine::scheduler::park_child(call.child, suspension)?;
            return Ok(RequestResult { status: OK,
                                      action: RequestAction::Replay });
        }
        call.state = query.index;
        call.code = query.exit_code;
        Ok(RequestResult::done(OK))
    });
    match result
    {
        Ok(Ok(reply)) => reply,
        Ok(Err(status)) => RequestResult::done(status),
        Err(error) => RequestResult::done(error.status())
    }
}

/// Dispatch native and classic process requests without holding desktop state
/// across storage reads, provider callbacks or scheduler waits.
pub(crate) fn request(call: &mut ProcessCall) -> Status
{
    request_inner(call, None)
}

pub(crate) fn request_with_argv(call: &mut ProcessCall, bytes: &[u8], length: usize, count: u32)
                                -> Status
{
    let Some((argv, path)) = bytes.split_at_checked(length)
    else
    {
        return BAD_ARGUMENT;
    };
    if call.operation != LAUNCH
       || call.streams == [0; 3]
       || count as usize > mac_abi::processes::ARGUMENT_COUNT
       || bytes.len() > mac_abi::processes::ARGUMENT_BYTES
       || argv.is_empty() != (count == 0)
       || (!argv.is_empty() && (argv.first() == Some(&0) || argv.last() != Some(&0)))
       || argv.iter().filter(|&&byte| byte == 0).count() != count as usize
       || core::str::from_utf8(argv).is_err()
       || path.contains(&0)
       || core::str::from_utf8(path).is_err()
    {
        return BAD_ARGUMENT;
    }
    request_inner(call, Some((bytes, length, count)))
}

pub(crate) fn startup_argv(output: &mut [u8], reserved: u32) -> Result<(u32, u32, u32), Status>
{
    if reserved != 0
    {
        return Err(BAD_ARGUMENT);
    }
    let identity = crate::context::current_thread_context().identity();
    crate::kernel::with_toolbox(|toolbox| {
        let process = toolbox.processes.get(identity.app.0)?;
        if !process.console() || process.info.thread != identity.thread.0
        {
            return Err(ACCESS_DENIED);
        }
        let Some(argv) = &process.argv
        else
        {
            return Ok((0, 0, 0));
        };
        let output = output.get_mut(..argv.bytes.len()).ok_or(NO_MEMORY)?;
        output.copy_from_slice(&argv.bytes);
        Ok((argv.length as u32, argv.count, (argv.bytes.len() - argv.length) as u32))
    }).map_err(|error| error.status())?
}

fn request_inner(call: &mut ProcessCall, argv: Option<(&[u8], usize, u32)>) -> Status
{
    call.requirement_class = [0; 4];
    call.requirement_version = 0;
    let result = (|| {
        if call.operation != LAUNCH
        {
            if launch::pending()
            {
                launch::abandon_thread(crate::context::current_thread());
                return Err(BAD_ABI);
            }
            if call.operation == FINDER_DESKTOP_READY
            {
                if call.id != 0 || call.index != 0 || call.volume != 0 || call.cnid != 0
                {
                    return Err(BAD_ARGUMENT);
                }
                finder_desktop_ready()?;
                return Ok(OK);
            }
            return crate::kernel::with_toolbox(|d| d.process(call)).map_err(|e| e.status());
        }
        crate::context::without_recovery(|| {
            if launch::pending()
            {
                return launch::resume(call, launch::Origin::Process).map(|()| OK);
            }
            // An app can quit and another launch it before the next timer tick.
            // This trap is already on the other app's stack: finish that real
            // checkpoint before deciding whether reloading the old image is safe.
            crate::machine::scheduler::reclaim_retired();
            retirement::pump();
            if call.streams == [0; 3] && !crate::toolbox::files::application::pending()
           && crate::kernel::with_toolbox(|d| d.activate_existing_application(call))
                .map_err(|e| e.status())??
        {
            return Ok(OK);
        }
            let owner = crate::context::current_thread_context().identity().app.0;
            if call.streams != [0; 3] && !crate::toolbox::files::application::pending()
            {
                crate::kernel::with_toolbox(|toolbox| toolbox.console_session(owner, call.id))
                    .map_err(|error| error.status())??;
            }
            let loaded = crate::toolbox::files::application::read(owner, call.volume, call.cnid,
                                                                  call.id, call.streams)?;
            launch_application(call, *loaded, launch::Origin::Process, argv).map(|()| OK)
        })
    })();
    result.unwrap_or_else(|error| error)
}

#[derive(Debug)]
pub(crate) enum ServiceAdmissionFailure
{
    Format(mac_formats::requirements::Error),
    Runtime(crate::toolbox::native_threads::Error),
    Provider
    {
        requirement: mac_formats::requirements::Requirement,
        cause:       crate::drivers::ProcessProviderError
    }
}
impl core::fmt::Display for ServiceAdmissionFailure
{
    fn fmt(&self, out: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::Format(cause) => write!(out, "invalid provider requirements: {cause}"),
            Self::Runtime(cause) => write!(out, "provider runtime scope: {cause}"),
            Self::Provider { requirement,
                             cause } =>
            {
                write!(out,
                       "required service {:?} version {}: {cause}",
                       requirement.class, requirement.version)
            }
        }
    }
}
impl ServiceAdmissionFailure
{
    pub(crate) fn status(&self) -> Status
    {
        match self
        {
            Self::Format(_) => CORRUPT_DATA,
            Self::Runtime(cause) => cause.status(),
            Self::Provider { cause,
                             .. } => cause.status()
        }
    }

    fn reply(&self, call: &mut ProcessCall)
    {
        if let Self::Provider { requirement,
                                .. } = self
        {
            call.requirement_class = requirement.class.0;
            call.requirement_version = requirement.version;
        }
    }
}

/// Validate declarations and retain exact live registrations in the authentic
/// Process runtime. The optional interface is the exact ENTR1 selection; all
/// application paths must complete its READY before publishing author threads.
pub(crate) fn select_required_services(resources: &[mac_formats::resource::Resource],
                                       owner: &crate::toolbox::handles::OwnerRef,
                                       allocation: crate::machine::scheduler::accounting::AllocationAuthority,
                                       tls: &crate::toolbox::native_threads::RootBinding)
                                       -> Result<Option<u64>, ServiceAdmissionFailure>
{
    let requirements =
        mac_formats::requirements::Requirements::from_resources(resources).map_err(|cause| {
            serial!("PROCESS_REQUIREMENTS_REFUSED app={} {cause}", owner.owner().incarnation());
            ServiceAdmissionFailure::Format(cause)
        })?;
    if requirements.iter().len() == 0
    {
        return Ok(None);
    }
    let scope = tls.service_scope(owner).map_err(ServiceAdmissionFailure::Runtime)?;
    let mut entropy = None;
    for requirement in requirements.iter()
    {
        let selected =
            crate::drivers::admit_process_provider(owner,
                                                   allocation.clone(),
                                                   &scope,
                                                   requirement.class,
                                                   requirement.version).map_err(|cause| {
                serial!("PROCESS_SERVICE_REFUSED app={} class={:?} version={} status={}; {cause}",
                        owner.owner().incarnation(),
                        requirement.class,
                        requirement.version,
                        cause.status());
                ServiceAdmissionFailure::Provider { requirement,
                                                    cause }
            })?;
        if requirement.class == mac_entropy_interface::CLASS
           && requirement.version == mac_entropy_interface::VERSION
        {
            entropy = Some(selected);
        }
    }
    Ok(entropy)
}

/// Ordinary Open and document delivery share this genuine launch coordinator.
/// No Desktop guard spans root preparation or provider admission.
fn launch_application(call: &mut ProcessCall, loaded: crate::toolbox::files::application::Loaded,
                      origin: launch::Origin, argv: Option<(&[u8], usize, u32)>)
                      -> Result<(), Status>
{
    crate::context::without_recovery(|| launch::start(call, loaded, origin, argv))
}

fn prepare_launch(call: &mut ProcessCall, loaded: crate::toolbox::files::application::Loaded,
                  argv: Option<(&[u8], usize, u32)>)
                  -> Result<Option<launch::Prepared>, Status>
{
    call.requirement_class = [0; 4];
    call.requirement_version = 0;
    let admission = crate::kernel::with_toolbox(|toolbox| {
                        toolbox.reserve_application(call, loaded.catalog)
                    }).map_err(|error| error.status())??;
    let Some(mut admission) = admission
    else
    {
        return Ok(None);
    };
    // A console child inherits its launcher's working directory; a GUI launch
    // keeps the Documents default. reserve_application already enforced that a
    // console session and a CLAP image imply each other.
    let console = loaded.catalog.file_type == *b"CLAP";
    let roots = (|| {
        let prepared = crate::toolbox::application_heap::prepare_root(
                admission.owner().clone(), admission.allocation()).map_err(|error| {
                    serial!("PROCESS_HEAP_REFUSED app={} {error}", admission.id());
                    error.status()
                })?;
        let heap = crate::toolbox::application_heap::publish_root(prepared).map_err(|failure| {
                       let status = failure.cause.status();
                       serial!("PROCESS_HEAP_PUBLICATION_REFUSED app={} {}",
                               admission.id(),
                               failure.cause);
                       failure.prepared.dispose();
                       status
                   })?;
        let prepared_tls =
            crate::toolbox::native_threads::prepare_root(admission.owner().clone(),
                                                         admission.allocation()).map_err(|cause| {
                serial!("PROCESS_TLS_REFUSED app={} {cause}", admission.id());
                cause.status()
            })?;
        let tls =
            crate::toolbox::native_threads::publish_root(prepared_tls).map_err(|failure| {
                let status = failure.cause.status();
                serial!("PROCESS_TLS_PUBLICATION_REFUSED app={} {}", admission.id(), failure.cause);
                failure.prepared.dispose();
                status
            })?;
        let selected = select_required_services(&loaded.resources,
                                                admission.owner(),
                                                admission.allocation(),
                                                &tls).map_err(|cause| {
                                                         serial!("PROCESS_ADMISSION_REFUSED \
                                                                  app={} {cause}",
                                                                 admission.id());
                                                         cause.reply(call);
                                                         cause.status()
                                                     })?;
        let ready = match selected
        {
            Some(provider) =>
            {
                Some(launch::ready_job(&tls, admission.owner(), provider).map_err(|status| {
                         call.requirement_class = mac_entropy_interface::CLASS.0;
                         call.requirement_version = mac_entropy_interface::VERSION;
                         status
                     })?)
            }
            None => None
        };
        let argv = if let Some((bytes, length, count)) = argv
        {
            Some(crate::allocator::with_target(crate::allocator::Target::Owner {
                account: admission.allocation(),
                class: mac_kernel_model::policy::MemoryClass::Kernel
            }, || {
                let mut owned = alloc::vec::Vec::new();
                owned.try_reserve_exact(bytes.len()).map_err(|_| NO_MEMORY)?;
                owned.extend_from_slice(bytes);
                Ok::<_, Status>(Arguments { bytes: owned, length, count })
            })?)
        }
        else
        {
            None
        };
        let caller_directory = if console { Some(crate::context::current_owner()?) } else { None };
        Ok::<_, Status>((heap, tls, ready, argv, caller_directory))
    })();
    match roots
    {
        Ok((heap, tls, ready, argv, caller_directory)) =>
        {
            Ok(Some(launch::Prepared { admission,
                                       heap,
                                       tls,
                                       loaded: Some(loaded),
                                       ready,
                                       argv,
                                       caller_directory,
                                       directory: None,
                                       directory_bound: false }))
        }
        Err(status) =>
        {
            launch::retire(&mut admission);
            Err(status)
        }
    }
}

fn finish_launch(call: &mut ProcessCall, prepared: &mut launch::Prepared, origin: launch::Origin)
                 -> Result<(), Status>
{
    let crate::toolbox::files::application::Loaded { catalog,
                                                     resources } =
        prepared.loaded.take().ok_or(BAD_ABI)?;
    let image = crate::allocator::with_target(crate::allocator::Target::Owner {
        account: prepared.admission.allocation(), class: mac_kernel_model::policy::MemoryClass::Kernel
    }, || crate::kernel::load_application(prepared.admission.owner(),
                                          &prepared.admission.allocation(),
                                          catalog.creator, catalog.file_type, resources))?;
    // READY has settled; admit the genuine private table hierarchy without a
    // Desktop borrow, then move its sole pin into actual THRD preparation.
    let space = crate::machine::paging::process::admit(prepared.admission.owner(),
                                                       &prepared.admission.allocation(),
                                                       &image).map_err(|cause| {
                                                                  serial!("PROCESS_SPACE_REFUSED \
                                                                           owner={} {cause}",
                                                                          prepared.admission.id());
                                                                  cause.status()
                                                              })?;
    crate::kernel::with_toolbox(|toolbox| {
        toolbox.processes.bind_heap(prepared.admission.owner(), prepared.heap.clone())?;
        toolbox.processes.bind_tls(prepared.admission.owner(), prepared.tls.clone())?;
        toolbox.processes.get_mut(prepared.admission.id())?.argv = prepared.argv.take();
        toolbox.finish_application_launch(call,
                                          catalog,
                                          image,
                                          space,
                                          &mut prepared.admission,
                                          matches!(origin, launch::Origin::Document(_)))
    }).map_err(|error| error.status())?
}

impl Default for Manager
{
    fn default() -> Self
    {
        Self::new()
    }
}

/// Non-consuming readiness for the caller's retained child result receipt.
pub(crate) fn child_readiness(child: u64) -> Result<u32, Status>
{
    let caller = crate::context::current_thread_context().identity().app.0;
    crate::kernel::with_toolbox(|t| {
        let mut query = ProcessCall { id: child,
                                      ..Default::default() };
        t.processes.child_status(caller, &mut query)?;
        Ok(if query.index == CHILD_RUNNING { 0 } else { mac_abi::readiness::READY })
    }).map_err(|e| e.status())?
}
