//! Resident shared, memory-backed Scrap Manager. The kernel's TryState serializes
//! all callers; no caller pointers or borrowed payloads survive a request.
#![deny(unsafe_code)]
use alloc::{string::String, sync::Arc, vec::Vec};

use mac_abi::{scrap::*, *};
use mac_object_table::shared::InvocationGate as _;

use crate::toolbox::handles::InvocationGate;

// The sole pointer boundary is isolated in this module.
#[allow(unsafe_code)]
pub mod native;
pub(crate) mod extension;
pub(crate) mod extension_wire;
#[cfg(target_arch = "riscv64")]
pub(crate) mod extension_runtime;
struct Record
{
    kind:   [u8; 4],
    bytes:  Payload,
    offset: u32
}
enum Payload
{
    Binary(Vec<u8>),
    Text(Arc<String>)
}
impl Payload
{
    fn bytes(&self) -> &[u8]
    {
        match self
        {
            Self::Binary(bytes) => bytes,
            Self::Text(text) => text.as_bytes()
        }
    }
}
/// A clipboard change version, not an object ID or publication authority. The
/// extension service still validates its owner/session grant at every request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Revision(u64);
impl Revision
{
    pub fn get(self) -> u64
    {
        self.0
    }
}
pub enum SnapshotContent<'a>
{
    Empty,
    Text(&'a str),
    Unsupported
}
#[derive(Clone)]
enum SharedContent
{
    Empty,
    Text(Arc<String>),
    Unsupported
}
/// Text ownership remains resident and immutable across asynchronous use. An
/// unchanged observer takes no snapshot; a changed snapshot only clones a pin.
#[derive(Clone)]
pub struct Snapshot
{
    revision: Revision,
    content:  SharedContent
}
impl Snapshot
{
    pub fn revision(&self) -> Revision
    {
        self.revision
    }

    pub fn content(&self) -> SnapshotContent<'_>
    {
        match &self.content
        {
            SharedContent::Empty => SnapshotContent::Empty,
            SharedContent::Text(text) => SnapshotContent::Text(text),
            SharedContent::Unsupported => SnapshotContent::Unsupported
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeError
{
    Conflict
    {
        expected: Revision,
        actual:   Revision
    },
    RevisionExhausted,
    PayloadLimit
    {
        bytes:   usize,
        maximum: usize
    },
    Allocation
}
impl core::fmt::Display for ChangeError
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::Conflict { .. } =>
            {
                f.write_str("Scrap Manager changed while this clipboard transfer was pending; \
                             keep the current Scrap and retry from a fresh snapshot")
            }
            Self::RevisionExhausted =>
            {
                f.write_str("Scrap Manager has exhausted this boot's change revisions; preserve \
                             clipboard text and restart System before retrying")
            }
            Self::PayloadLimit { bytes,
                                 maximum } =>
            {
                write!(f,
                       "Scrap Manager text needs {bytes} bytes but the clipboard permits \
                        {maximum}; retain the current Scrap and copy a smaller selection")
            }
            Self::Allocation =>
            {
                f.write_str("Scrap Manager could not reserve immutable text storage; retain the \
                             current Scrap, close unused applications and retry")
            }
        }
    }
}
impl ChangeError
{
    fn status(self) -> Status
    {
        match self
        {
            Self::Conflict { .. } => SCAN_STALE,
            Self::RevisionExhausted => NOT_SUPPORTED,
            Self::PayloadLimit { .. } | Self::Allocation => NO_MEMORY
        }
    }
}
pub struct Manager
{
    records:    Vec<Record>,
    size:       u32,
    count:      u16,
    revision:   Revision,
    extensions: Option<extension::Service>
}
impl Default for Manager
{
    fn default() -> Self
    {
        Self { records:    Vec::new(),
               size:       0,
               count:      0,
               revision:   Revision(1),
               extensions: None }
    }
}
impl Manager
{
    /// Replace the shared clipboard in one transaction for native editor Copy.
    /// Allocation and record construction finish before the old generation is
    /// released, so an error preserves the previous clipboard in full.
    pub fn replace_text(&mut self, text: &str) -> Result<(), Status>
    {
        self.replace(text).map(|_| ()).map_err(ChangeError::status)
    }

    pub fn revision(&self) -> Revision
    {
        self.revision
    }

    pub fn snapshot(&self) -> Snapshot
    {
        let content = if self.records.is_empty()
        {
            SharedContent::Empty
        }
        else
        {
            self.records
                .iter()
                .find_map(|record| {
                    match &record.bytes
                    {
                        Payload::Text(text) => Some(SharedContent::Text(text.clone())),
                        Payload::Binary(_) => None
                    }
                })
                .unwrap_or(SharedContent::Unsupported)
        };
        Snapshot { revision: self.revision,
                   content }
    }

    /// This primitive compares only content versions. Its resident extension
    /// caller must separately retain and validate the authenticated write grant.
    pub fn compare_replace_text(&mut self, expected: Revision, text: &str)
                                -> Result<Revision, ChangeError>
    {
        if expected != self.revision
        {
            return Err(ChangeError::Conflict { expected,
                                               actual: self.revision });
        }
        self.replace(text)
    }

    fn next_revision(&self) -> Result<Revision, ChangeError>
    {
        self.revision.0.checked_add(1).map(Revision).ok_or(ChangeError::RevisionExhausted)
    }

    fn replace(&mut self, text: &str) -> Result<Revision, ChangeError>
    {
        let revision = self.next_revision()?;
        let record = Self::text_record(text, 8)?;
        let size = 8 + text.len() + (text.len() & 1);
        let mut records = Vec::new();
        InvocationGate::with_system_allocation(|| records.try_reserve_exact(1))
            .map_err(|_| ChangeError::Allocation)?;
        records.push(record);
        // All storage and version admission completed before the old clipboard
        // or its classic clear count is touched. No rollback is necessary.
        self.records = records;
        self.size = size as u32;
        self.count = self.count.wrapping_add(1);
        self.revision = revision;
        self.extension_changed();
        Ok(revision)
    }

    fn text_record(text: &str, offset: u32) -> Result<Record, ChangeError>
    {
        let maximum = MAX_BYTES - 8;
        if text.len() > maximum
        {
            return Err(ChangeError::PayloadLimit { bytes: text.len(),
                                                   maximum });
        }
        // The resident Scrap owns accepted content independently of the sender.
        // Charge both immutable backing and its sharing metadata to System.
        let bytes = InvocationGate::with_system_allocation(|| {
            let mut bytes = String::new();
            bytes.try_reserve_exact(text.len()).map_err(|_| ChangeError::Allocation)?;
            bytes.push_str(text);
            Arc::try_new(bytes).map_err(|_| ChangeError::Allocation)
        })?;
        Ok(Record { kind: TEXT,
                    bytes: Payload::Text(bytes),
                    offset })
    }

    /// Borrow the first native text record while holding the Scrap guard.
    pub fn text(&self) -> Result<&str, Status>
    {
        let record = self.records.iter().find(|record| record.kind == TEXT).ok_or(NO_TYPE_ERR)?;
        match &record.bytes
        {
            Payload::Text(text) => Ok(text),
            Payload::Binary(_) => Err(BAD_ARGUMENT)
        }
    }

    pub fn request(&mut self, call: &mut ScrapCall, input: &[u8], output: &mut [u8]) -> Status
    {
        call.transferred = 0;
        let result =
            match call.operation
            {
                INFO => Ok(()),
                CLEAR =>
                {
                    match self.next_revision()
                    {
                        Ok(revision) =>
                        {
                            self.records.clear();
                            self.size = 0;
                            self.count = self.count.wrapping_add(1);
                            self.revision = revision;
                            self.extension_changed();
                            Ok(())
                        }
                        Err(error) => Err(error.status())
                    }
                }
                PUT =>
                {
                    if call.kind == TEXT { Err(BAD_ARGUMENT) } else { self.put(call.kind, input) }
                }
                PUT_TEXT =>
                {
                    match core::str::from_utf8(input)
                    {
                        Ok(text) => self.put_text(text),
                        Err(_) => Err(BAD_ARGUMENT)
                    }
                }
                FLAVOR_AT =>
                {
                    match self.records.get(call.index as usize)
                    {
                        Some(record) =>
                        {
                            call.flavor = record.info();
                            Ok(())
                        }
                        None => Err(NOT_FOUND)
                    }
                }
                READ | READ_TEXT => self.read(call, output),
                LOAD | UNLOAD => Err(NOT_SUPPORTED),
                _ => Err(NOT_SUPPORTED)
            };
        call.info = ScrapInfo { size:    self.size,
                                count:   self.count,
                                flavors: self.records.len() as u16 };
        result.map_or_else(|error| error, |_| OK)
    }

    fn put(&mut self, kind: [u8; 4], input: &[u8]) -> Result<(), Status>
    {
        if kind == TEXT
        {
            return Err(BAD_ARGUMENT);
        }
        let revision = self.next_revision().map_err(ChangeError::status)?;
        let size = (self.size as usize).checked_add(8)
                                       .and_then(|size| size.checked_add(input.len()))
                                       .and_then(|size| size.checked_add(input.len() & 1))
                                       .ok_or(NO_MEMORY)?;
        if size > MAX_BYTES || self.records.len() >= MAX_FLAVORS
        {
            return Err(NO_MEMORY);
        }
        // Reserve and copy into staging before touching logical scrap state.
        // Fallible reservation never loses existing records or increments count.
        let mut bytes = Vec::new();
        InvocationGate::with_system_allocation(|| bytes.try_reserve_exact(input.len()))
            .map_err(|_| NO_MEMORY)?;
        bytes.extend_from_slice(input);
        InvocationGate::with_system_allocation(|| self.records.try_reserve(1)).map_err(|_| {
                                                                                  NO_MEMORY
                                                                              })?;
        self.records.push(Record { kind,
                                   bytes: Payload::Binary(bytes),
                                   offset: self.size + 8 });
        self.size = size as u32;
        self.revision = revision;
        self.extension_changed();
        Ok(())
    }

    fn put_text(&mut self, text: &str) -> Result<(), Status>
    {
        let revision = self.next_revision().map_err(ChangeError::status)?;
        let size = (self.size as usize).checked_add(8)
                                       .and_then(|size| size.checked_add(text.len()))
                                       .and_then(|size| size.checked_add(text.len() & 1))
                                       .ok_or(NO_MEMORY)?;
        if size > MAX_BYTES || self.records.len() >= MAX_FLAVORS
        {
            return Err(NO_MEMORY);
        }
        let record = Self::text_record(text, self.size + 8).map_err(ChangeError::status)?;
        InvocationGate::with_system_allocation(|| self.records.try_reserve(1)).map_err(|_| {
                                                                                  NO_MEMORY
                                                                              })?;
        self.records.push(record);
        self.size = size as u32;
        self.revision = revision;
        self.extension_changed();
        Ok(())
    }

    fn read(&self, call: &mut ScrapCall, output: &mut [u8]) -> Result<(), Status>
    {
        let text = call.operation == READ_TEXT;
        if !text && call.kind == TEXT || text && call.offset != 0
        {
            return Err(BAD_ARGUMENT);
        }
        let kind = if text { TEXT } else { call.kind };
        let record = self.records.iter().find(|record| record.kind == kind).ok_or(NO_TYPE_ERR)?;
        call.flavor = record.info();
        if text
        {
            if output.len() < record.bytes.bytes().len()
            {
                return Err(NO_MEMORY);
            }
            output[..record.bytes.bytes().len()].copy_from_slice(record.bytes.bytes());
            call.transferred = record.bytes.bytes().len();
        }
        else
        {
            let input = record.bytes.bytes().get(call.offset as usize..).ok_or(BAD_ARGUMENT)?;
            let length = input.len().min(output.len());
            output[..length].copy_from_slice(&input[..length]);
            call.transferred = length;
        }
        Ok(())
    }
}
impl Record
{
    fn info(&self) -> FlavorInfo
    {
        FlavorInfo { kind:   self.kind,
                     length: self.bytes.bytes().len() as u32,
                     offset: self.offset }
    }
}

#[cfg(test)]
mod revision_tests
{
    use super::*;
    #[test]
    fn last_change_is_usable_and_exhaustion_preserves_every_committed_field()
    {
        let mut manager = Manager::default();
        manager.replace_text("old").unwrap();
        manager.revision = Revision(u64::MAX - 1);
        let last = manager.compare_replace_text(manager.revision(), "final").unwrap();
        assert_eq!(last.get(), u64::MAX);
        let count = manager.count;
        let size = manager.size;
        for operation in [CLEAR, PUT_TEXT, PUT]
        {
            let mut call = ScrapCall { operation,
                                       kind: *b"BINA",
                                       ..Default::default() };
            assert_eq!(manager.request(&mut call, b"new", &mut []), NOT_SUPPORTED);
            assert_eq!(manager.revision(), last);
            assert_eq!((manager.count, manager.size), (count, size));
            assert_eq!(manager.text().unwrap(), "final");
        }
        let cause = manager.compare_replace_text(last, "").unwrap_err();
        assert_eq!(cause, ChangeError::RevisionExhausted);
        let cause = alloc::string::ToString::to_string(&cause);
        for expected in ["Scrap Manager", "exhausted", "restart"]
        {
            assert!(cause.contains(expected), "{cause}");
        }
        assert_eq!(manager.revision(), last);
        assert_eq!(manager.text().unwrap(), "final");
    }
}
