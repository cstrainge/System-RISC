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
/// Remembered "this application has no icon for that document type" answers,
/// so a folder of such documents reads the application's bundle once.
const MAX_ABSENT: usize = 4096;
/// The kind of an entry holding a file's own icon.
pub(super) const OWN: [u8; 4] = [0; 4];

struct Entry
{
    volume:   u32,
    parent:   u32,
    file:     u32,
    /// `OWN` for the file's own icon; otherwise the document type this icon is
    /// the application's answer for.
    kind:     [u8; 4],
    family:   Weak<IconFamily>,
    retained: Option<Arc<IconFamily>>,
    bytes:    usize,
    /// A document pointing at its application's icon, not an icon read from
    /// this file. The link is only as good as the answer to "who owns this
    /// creator".
    alias:    bool
}

#[derive(Default)]
pub(super) struct Cache
{
    entries: Vec<Entry>,
    bytes:   usize,
    absent:  Vec<(u32, u32, [u8; 4])>
}
impl Cache
{
    pub fn get(&self, volume: u32, file: u32) -> Option<Arc<IconFamily>>
    {
        self.get_kind(volume, file, OWN)
    }

    pub fn get_kind(&self, volume: u32, file: u32, kind: [u8; 4]) -> Option<Arc<IconFamily>>
    {
        self.entries
            .iter()
            .find(|entry| entry.volume == volume && entry.file == file && entry.kind == kind)
            .and_then(|entry| entry.family.upgrade())
    }

    pub fn insert(&mut self, volume: u32, parent: u32, file: u32, family: Arc<IconFamily>)
                  -> Result<(), Status>
    {
        self.insert_kind(volume, parent, file, OWN, family)
    }

    /// Cache and retain a decoded family under `(volume, file, kind)`.
    pub fn insert_kind(&mut self, volume: u32, parent: u32, file: u32, kind: [u8; 4],
                       family: Arc<IconFamily>)
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
                                  kind,
                                  family: Arc::downgrade(&family),
                                  retained: Some(family),
                                  bytes,
                                  alias: false });
        self.bytes += bytes;
        Ok(())
    }

    /// Point a document at a family another entry already retains. The alias
    /// owns no pixels; it lets the document's next draw skip the lookup. A
    /// full index is not an error here: the document simply resolves again.
    pub fn alias(&mut self, volume: u32, parent: u32, file: u32, family: &Arc<IconFamily>)
    {
        self.entries.retain(|entry| entry.family.strong_count() != 0);
        if self.entries.len() >= MAX_ENTRIES || self.entries.try_reserve(1).is_err()
        {
            return;
        }
        self.entries.push(Entry { volume,
                                  parent,
                                  file,
                                  kind: OWN,
                                  family: Arc::downgrade(family),
                                  retained: None,
                                  bytes: 0,
                                  alias: true });
    }

    pub fn absent(&self, volume: u32, file: u32, kind: [u8; 4]) -> bool
    {
        self.absent.contains(&(volume, file, kind))
    }

    pub fn note_absent(&mut self, volume: u32, file: u32, kind: [u8; 4])
    {
        if self.absent.len() >= MAX_ABSENT
        {
            self.absent.clear();
        }
        if self.absent.try_reserve(1).is_ok()
        {
            self.absent.push((volume, file, kind));
        }
    }

    /// An application arrived, so who owns a creator may have a new answer.
    /// Forget what was derived from the old one: the misses and the links from
    /// documents to owners. Every icon that was read stays.
    pub fn forget_owners(&mut self)
    {
        self.absent.clear();
        self.entries.retain(|entry| !entry.alias);
    }

    pub fn invalidate(&mut self, volume: u32, directory: u32)
    {
        // An absent answer was read from an application on this volume; any
        // change there may have changed it.
        self.absent.retain(|(absent, ..)| *absent != volume);
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
