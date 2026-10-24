//! Application metadata and activation policy, independent of CPU scheduling.
#![forbid(unsafe_code)]

use mac_abi::{processes::*, *};

use crate::toolbox::handles::{Directory, DirectoryError, DomainKind, OwnerIndex};
pub mod menu;
pub(crate) mod documents;
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
pub struct Process
{
    pub info:  ProcessInfo,
    pub image: usize,
    pub stamp: u64
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
        crate::kernel::image(self.image).map_or(0, |image| image.authority)
    }
}
pub struct Manager
{
    entries:           Directory<Process>,
    documents:         crate::toolbox::handles::Table<documents::Batch>,
    document_sequence: u64,
    stamp:             u64
}
impl Manager
{
    pub(crate) fn inspect(&self, caller: u64, call: &mut mac_abi::inspection::Call) -> Status
    {
        crate::toolbox::inspection::request(&self.entries, caller, call)
    }

    pub fn new() -> Self
    {
        Self { entries: Directory::new(),
               documents:crate::toolbox::handles::Table::new(crate::toolbox::handles::Kind::DocumentBatches),document_sequence:0,
               stamp:   0 }
    }

    pub(crate) fn ledger(&self, id: u64) -> Result<crate::toolbox::handles::OwnerRef, Status>
    {
        self.entries.get(id).map(|entry| entry.ledger().clone()).ok_or(NOT_FOUND)
    }

    pub fn len(&self) -> usize
    {
        self.iter().count()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Process>
    {
        // Dying/quarantined domains remain inspectable until their death
        // barrier closes, but are no longer running applications.
        self.entries.iter().filter(|entry| entry.ledger().accepting()).map(|entry| entry.value())
    }

    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut Process>
    {
        self.entries.values_mut()
    }

    pub fn add(&mut self, name: &str, volume: u32, cnid: u32, image: usize) -> Result<u64, Status>
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
        let ledger = OwnerIndex::issue(DomainKind::Process).map_err(|error| {
                                                               serial!("PROCESS_DOMAIN_REFUSED: \
                                                                        {error}");
                                                               NO_MEMORY
                                                           })?;
        let id = ledger.owner().incarnation();
        info.id = id;
        self.entries
            .insert(ledger,
                    Process { info,
                              image,
                              stamp: 0 })
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

    pub fn remove(&mut self, id: u64) -> Result<(), Status>
    {
        self.entries.remove(id).map(|_| ()).map_err(|error| {
                                               serial!("PROCESS_DOMAIN_RETIRE_REFUSED: {error}");
                                               match error
                                               {
                                                   DirectoryError::NotFound { .. } => NOT_FOUND,
                                                   DirectoryError::LiveResources { .. }
                                                   | DirectoryError::PendingResources { .. } =>
                                                   {
                                                       SERVICE_BUSY
                                                   }
                                                   DirectoryError::Storage { .. } => NO_MEMORY
                                               }
                                           })
    }

    pub fn snapshot(&self, id: u64) -> Result<ProcessInfo, Status>
    {
        let mut info = self.get(id)?.info;
        info.switches = crate::machine::scheduler::switches(id);
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
