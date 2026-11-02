//! GUI Documents-directory admission. The prospective process owns a real
//! catalog alias before any provider callback; cancellation retains that pin
//! through the actual I/O Subject lifetime, never through a fabricated ID lease.
#![forbid(unsafe_code)]
use mac_filesystem_interface::{Arguments, Operation as SourceOperation};
use mac_kernel_model::policy::MemoryClass;

use super::*;
use crate::{allocation::Target,
            drivers::{OwnedJob, jobs::Failure},
            machine::scheduler::accounting::AllocationAuthority};

pub(crate) struct LaunchDirectory
{
    // Drop/cancel the real callback before releasing our Subject copy.
    job:      Option<OwnedJob>,
    subject:  Subject,
    owner:    OwnerRef,
    volume:   u32,
    parent:   u32,
    provider: Option<u64>,
    state:    Arc<pins::DirectoryState>,
    complete: bool
}
impl Drop for LaunchDirectory
{
    fn drop(&mut self)
    {
        if !self.complete
        {
            self.state.cancelled.store(true, Ordering::Release);
            pins::directory_changed();
        }
        // OwnedJob Drop requests cancellation. The queue retains its genuine
        // Subject until terminal ACK; Role::Directory defers alias cleanup until
        // that Subject's real scope closes. No manager re-entry from Drop.
    }
}
fn files<R>(f: impl FnOnce(&mut Manager) -> Result<R, Status>) -> Result<R, Status>
{
    crate::kernel::with_files(f).map_err(|error| error.status())?
}
impl LaunchDirectory
{
    /// Call after READY and before publishing author execution, outside Desktop.
    /// The startup/user Documents directory is independent of executable
    /// location. Original executable metadata/pins remain separate authority.
    pub(crate) fn prepare(owner: &OwnerRef, allocation: AllocationAuthority)
                          -> Result<Self, Status>
    {
        if !owner.accepting() || allocation.owner() != owner.owner()
        {
            return Err(ACCESS_DENIED);
        }
        let subject=crate::drivers::jobs::subjects::resident()?.admit(&subject::Authority::Client(owner.clone()),Target::Owner {account:allocation,class:MemoryClass::Kernel})?;
        crate::context::with_allocation(subject.allocation().clone(), || {
            let state = Arc::try_new(pins::DirectoryState::new()).map_err(|_| NO_MEMORY)?;
            let kinds = crate::toolbox::handles::file_pin_kinds()?;
            let system = crate::toolbox::handles::system_owner()?;
            let (volume, provider) = files(|manager| {
                manager.validate_file_authority(owner.owner().incarnation(), owner)?;
                manager.prepare_executable_pins(system, kinds)?;
                let mut startup = None;
                for (id, mounted) in manager.volumes.iter()
                {
                    if mounted.info().is_some_and(|info| info.startup != 0)
                    {
                        if startup.replace(id).is_some()
                        {
                            return Err(BAD_ABI);
                        }
                    }
                }
                let volume = startup.ok_or(NOT_FOUND)?;
                let provider = if matches!(manager.volumes.get(volume), Some(Mounted::Provider(_)))
                {
                    manager.read_provider(volume)?
                }
                else
                {
                    None
                };
                manager.validate_transfer_read(owner.owner().incarnation(), volume, ROOT_ID)?;
                // Protect the actual mounted root before resolving Documents.
                // Publish the genuine Documents alias before retiring this one.
                manager.pins.begin_directory(owner,
                                              volume,
                                              ROOT_ID,
                                              subject.scope().clone(),
                                              state.clone())?;
                Ok((volume, provider))
            })?;
            let mut binding = Self { job: None,
                                     subject,
                                     owner: owner.clone(),
                                     volume,
                                     parent: ROOT_ID,
                                     provider,
                                     state,
                                     complete: false };
            if let Some(provider) = provider
            {
                binding.job =
                    Some(provider::runtime::submit_args(&binding.subject,
                                                        provider,
                                                        SourceOperation::Lookup,
                                                        Arguments { volume: binding.volume,
                                                                    parent: ROOT_ID,
                                                                    payload: b"Documents",
                                                                    ..Default::default() })?);
            }
            Ok(binding)
        })
    }

    /// Real provider notification only; the number is not operation authority.
    pub(crate) fn notification_id(&self) -> Option<u64>
    {
        self.job.as_ref().map(OwnedJob::notification_id)
    }

    pub(crate) fn defer_current(&self) -> Result<(), Status>
    {
        self.job.as_ref().ok_or(WRONG_THREAD)?.defer_current()
    }

    /// One bounded turn. On completion/failure this consumes the admission so
    /// no permanent current-directory entry retains a live DIOP root.
    pub(crate) fn advance(binding: &mut Option<Self>) -> Result<bool, Status>
    {
        let directory = binding.as_mut().ok_or(BAD_ABI)?;
        let allocation = directory.subject.allocation().clone();
        // Final directory state and alias backing belong to the prospective
        // process, including after device I/O restores the launching caller.
        let result = crate::context::with_allocation(allocation, || directory.turn());
        if result != Ok(false)
        {
            // Dispose the admission after restoring the caller's selection.
            drop(binding.take());
        }
        result
    }

    fn turn(&mut self) -> Result<bool, Status>
    {
        if !self.owner.accepting()
        {
            return Err(OPERATION_CANCELLED);
        }
        let entry = if let Some(job) = self.job.as_mut()
        {
            let Some(completed) = job.run_once()?
            else
            {
                return Ok(false);
            };
            let bytes = completed.result.map_err(|failure| {
                                             match failure
                                             {
                                                 Failure::Service(status) => status,
                                                 Failure::Cancelled => OPERATION_CANCELLED
                                             }
                                         })?;
            provider::catalog(self.volume, &bytes, None, Some(ROOT_ID))?
        }
        else
        {
            let call = FileCall { operation: LOOKUP,
                                  volume: self.volume,
                                  cnid: self.parent,
                                  ..Default::default() };
            let input = Arc::try_new(Input { call:  Call::from(&call),
                                             name:  {
                                                 let mut name = String::new();
                                                 name.try_reserve_exact(9)
                                                     .map_err(|_| NO_MEMORY)?;
                                                 name.push_str("Documents");
                                                 name
                                             },
                                             bytes: Vec::new(),
                                             when:  0 }).map_err(|_| NO_MEMORY)?;
            let subject = self.subject.clone();
            let owner = self.owner.clone();
            // No moved device crosses a fallible context switch. The actual
            // canonical mounted slot retains any refused publication custody.
            let result =
                crate::machine::io::run_owned(input, |input| {
                    crate::context::with_bootstrap_io_subject(Some(subject.clone()), || {
                        crate::context::with_allocation(subject.allocation().clone(), || {
                            let work = files(|manager| {
                                manager.begin_local(owner.owner().incarnation(),
                                                    &owner,
                                                    subject.clone(),
                                                    input,
                                                    None)
                            })?;
                            let operation = work.operation.clone();
                            work.execute();
                            files(|manager| manager.local_ready())?;
                            Ok::<_, Status>(operation)
                        })
                    })
                }).map_err(|(error, _input)| error.status())?;
            let operation = result?;
            let result = operation.completed.lock().take().ok_or(BAD_ABI)?;
            match result?
            {
                Reply::Directory(entry) => entry,
                _ => return Err(BAD_ABI)
            }
        };
        if entry.parent != ROOT_ID || entry.volume != self.volume || !entry.is_directory()
        {
            return Err(BAD_ABI);
        }
        let state = Arc::try_new(pins::DirectoryState::new()).map_err(|_| NO_MEMORY)?;
        files(|manager| {
            manager.validate_file_authority(self.owner.owner().incarnation(), &self.owner)?;
            let provider = if matches!(manager.volumes.get(self.volume), Some(Mounted::Provider(_)))
            {
                manager.read_provider(self.volume)?
            }
            else
            {
                None
            };
            if provider != self.provider
            {
                return Err(BAD_ABI);
            }
            manager.pins.replace_directory(&self.owner,
                                           pins::File { volume: entry.volume,
                                                        id:     entry.id },
                                           self.subject.scope().clone(),
                                           state.clone())
        })?;
        self.parent = entry.id;
        self.state = state;
        self.complete = true;
        serial!("LAUNCH_DIRECTORY_READY owner={} volume={} node={} policy=Documents",
                self.owner.owner().incarnation(),
                entry.volume,
                entry.id);
        Ok(true)
    }
}
