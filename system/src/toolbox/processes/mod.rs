//! Application metadata and activation policy, independent of CPU scheduling.
#![forbid(unsafe_code)]

use mac_abi::{processes::*, *};
pub mod menu;
pub(crate) mod documents;
pub struct Process
{
    pub info:  ProcessInfo,
    pub image: usize,
    pub stamp: u64
}
pub struct Manager
{
    entries:           crate::toolbox::handles::Table<Process>,
    documents:         crate::toolbox::handles::Table<documents::Batch>,
    document_sequence: u64,
    stamp:             u64
}
impl Manager
{
    pub fn new() -> Self
    {
        Self { entries:
                   crate::toolbox::handles::Table::new(crate::toolbox::handles::Kind::Processes),
               documents:crate::toolbox::handles::Table::new(crate::toolbox::handles::Kind::DocumentBatches),document_sequence:0,
               stamp:   0 }
    }

    pub fn len(&self) -> usize
    {
        self.entries.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Process>
    {
        self.entries.iter().map(|(_, process)| process)
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
        let id = self.entries
                     .insert(Process { info,
                                       image,
                                       stamp: 0 })
                     .map_err(|_| NO_MEMORY)?;
        self.entries.get_mut(id).ok_or(NOT_FOUND)?.info.id = u64::from(id);
        Ok(u64::from(id))
    }

    pub fn get(&self, id: u64) -> Result<&Process, Status>
    {
        self.entries.get(u32::try_from(id).map_err(|_| NOT_FOUND)?).ok_or(NOT_FOUND)
    }

    pub fn get_mut(&mut self, id: u64) -> Result<&mut Process, Status>
    {
        self.entries.get_mut(u32::try_from(id).map_err(|_| NOT_FOUND)?).ok_or(NOT_FOUND)
    }

    pub fn activate(&mut self, id: u64) -> Result<(), Status>
    {
        self.get(id)?;
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

    pub fn remove(&mut self, id: u64)
    {
        if let Ok(id) = u32::try_from(id)
        {
            self.entries.remove(id);
        }
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
