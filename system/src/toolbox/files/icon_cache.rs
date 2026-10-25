//! Decoded file icons, scoped to their containing folder's change notifications.
//! Keys are metadata, never object authorities: the mounted volume is validated
//! before a hit. Retained drawings share immutable pixels through Arc.
#![forbid(unsafe_code)]
use alloc::{sync::{Arc, Weak},
            vec::Vec};

use mac_abi::{NO_MEMORY, Status};

use crate::toolbox::quickdraw::icons::IconFamily;

const MAX_BYTES: usize = 1024 * 1024;
// Eight Finder folders, each with at most 512 catalog entries. Weak index
// entries do not keep pixels alive after either the cache or scene drops them.
const MAX_ENTRIES: usize = 4096;

struct Entry
{
    volume:   u32,
    parent:   u32,
    file:     u32,
    family:   Weak<IconFamily>,
    retained: Option<Arc<IconFamily>>,
    bytes:    usize
}

#[derive(Default)]
pub(super) struct Cache
{
    entries: Vec<Entry>,
    bytes:   usize
}
impl Cache
{
    pub fn get(&self, volume: u32, file: u32) -> Option<Arc<IconFamily>>
    {
        self.entries
            .iter()
            .find(|entry| entry.volume == volume && entry.file == file)
            .and_then(|entry| entry.family.upgrade())
    }

    pub fn insert(&mut self, volume: u32, parent: u32, file: u32, family: Arc<IconFamily>)
                  -> Result<(), Status>
    {
        let bytes =
            family.images.iter().map(|image| image.pixels.len() * 4 + image.mask.len()).sum();
        self.entries.retain(|entry| entry.family.strong_count() != 0);
        if bytes > MAX_BYTES
           || self.entries.len() >= MAX_ENTRIES
           || self.entries.try_reserve(1).is_err()
        {
            return Err(NO_MEMORY);
        }
        while self.bytes + bytes > MAX_BYTES
        {
            let Some(entry) = self.entries.iter_mut().find(|entry| entry.retained.is_some())
            else
            {
                break;
            };
            entry.retained = None;
            self.bytes -= entry.bytes;
        }
        self.entries.push(Entry { volume,
                                  parent,
                                  file,
                                  family: Arc::downgrade(&family),
                                  retained: Some(family),
                                  bytes });
        self.bytes += bytes;
        Ok(())
    }

    pub fn invalidate(&mut self, volume: u32, directory: u32)
    {
        self.entries.retain(|entry| {
                        let keep =
                            entry.volume != volume || (directory != 0 && entry.parent != directory);
                        if !keep && entry.retained.is_some()
                        {
                            self.bytes -= entry.bytes;
                        }
                        keep
                    });
    }
}
