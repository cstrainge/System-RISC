//! Application metadata and activation policy, independent of CPU scheduling.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use mac_abi::{processes::*, *};
pub mod menu;
pub struct Process
{
    pub info:  ProcessInfo,
    pub image: usize,
    pub stamp: u64
}
pub struct Manager
{
    pub entries: Vec<Process>,
    next:        u64,
    stamp:       u64
}
impl Manager
{
    pub fn new() -> Self
    {
        Self { entries: Vec::new(),
               next:    1,
               stamp:   0 }
    }

    pub fn add(&mut self, name: &str, volume: u32, cnid: u32, image: usize) -> Result<u64, Status>
    {
        if self.entries.len() == 8 || self.next > u32::MAX as u64
        {
            return Err(NO_MEMORY);
        }
        let id = self.next;
        self.next += 1;
        let mut info = ProcessInfo { id,
                                     volume,
                                     cnid,
                                     ..Default::default() };
        if name.len() > info.name.len()
        {
            return Err(BAD_ARGUMENT);
        }
        info.name[..name.len()].copy_from_slice(name.as_bytes());
        info.name_length = name.len() as u32;
        self.entries.push(Process { info,
                                    image,
                                    stamp: 0 });
        Ok(id)
    }

    pub fn get(&self, id: u64) -> Result<&Process, Status>
    {
        self.entries.iter().find(|p| p.info.id == id).ok_or(NOT_FOUND)
    }

    pub fn get_mut(&mut self, id: u64) -> Result<&mut Process, Status>
    {
        self.entries.iter_mut().find(|p| p.info.id == id).ok_or(NOT_FOUND)
    }

    pub fn activate(&mut self, id: u64) -> Result<(), Status>
    {
        self.get(id)?;
        self.stamp += 1;
        let stamp = self.stamp;
        for p in &mut self.entries
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
        self.entries
            .iter()
            .filter(|p| p.info.id != excluding && p.info.hidden == 0)
            .max_by_key(|p| p.stamp)
            .map_or(1, |p| p.info.id)
    }

    pub fn remove(&mut self, id: u64)
    {
        self.entries.retain(|p| p.info.id != id);
    }

    pub fn snapshot(&self, id: u64) -> Result<ProcessInfo, Status>
    {
        let mut info = self.get(id)?.info;
        info.switches = crate::machine::scheduler::switches(id);
        Ok(info)
    }
}
/// Read a resource fork through the filesystem service on its actual I/O thread.
pub fn read_application(
    volume: u32, cnid: u32)
    -> Result<(mac_abi::files::CatalogInfo, alloc::vec::Vec<mac_formats::resource::Resource>),
              Status>
{
    crate::kernel::with_files(|files| files.application_resources(volume, cnid)).map_err(|e| {
                                                                                    e.status()
                                                                                })?
}

impl Default for Manager
{
    fn default() -> Self
    {
        Self::new()
    }
}
