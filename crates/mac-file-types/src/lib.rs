//! Versioned preference data classifies untyped documents. Installed drivers
//! separately register what they can open; this registry grants no open action.
#![no_std]
#![forbid(unsafe_code)]
extern crate alloc;
use alloc::{string::String, vec::Vec};

use serde::Deserialize;
pub const MAX_BYTES: usize = 64 * 1024;
pub const MAX_ASSOCIATIONS: usize = 128;
pub const DEFAULT_JSON: &[u8] = include_bytes!("../default.json");
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error
{
    Malformed,
    Version,
    Limit,
    Duplicate
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document
{
    version:      u32,
    associations: Vec<Row>
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Row
{
    extension: String,
    file_type: String,
    creator:   String
}
pub struct Registry
{
    entries: Vec<Entry>
}
struct Entry
{
    extension: [u8; 16],
    length:    usize,
    file_type: [u8; 4],
    creator:   [u8; 4]
}
impl Registry
{
    pub fn decode(bytes: &[u8]) -> Result<Self, Error>
    {
        if bytes.len() > MAX_BYTES
        {
            return Err(Error::Limit);
        }
        let doc: Document = serde_json::from_slice(bytes).map_err(|_| Error::Malformed)?;
        if doc.version != 1
        {
            return Err(Error::Version);
        }
        if doc.associations.len() > MAX_ASSOCIATIONS
        {
            return Err(Error::Limit);
        }
        let mut entries: Vec<Entry> = Vec::new();
        entries.try_reserve_exact(doc.associations.len()).map_err(|_| Error::Limit)?;
        for row in doc.associations
        {
            if row.extension.is_empty()
               || row.extension.len() > 16
               || !row.extension.bytes().all(|b| b.is_ascii_alphanumeric())
            {
                return Err(Error::Malformed);
            }
            let mut extension = [0; 16];
            extension[..row.extension.len()].copy_from_slice(row.extension.as_bytes());
            extension.make_ascii_lowercase();
            if entries.iter().any(|e| e.extension == extension)
            {
                return Err(Error::Duplicate);
            }
            fn code(text: &str) -> Result<[u8; 4], Error>
            {
                if !text.bytes().all(|b| b.is_ascii_graphic() || b == b' ')
                {
                    return Err(Error::Malformed);
                }
                text.as_bytes().try_into().map_err(|_| Error::Malformed)
            }
            entries.push(Entry { extension,
                                 length: row.extension.len(),
                                 file_type: code(&row.file_type)?,
                                 creator: code(&row.creator)? });
        }
        Ok(Self { entries })
    }

    pub fn classify(&self, name: &str, file_type: [u8; 4], creator: [u8; 4]) -> ([u8; 4], [u8; 4])
    {
        if file_type != [0; 4] && file_type != *b"BINA"
        {
            return (file_type, creator);
        }
        let Some((_, extension)) = name.rsplit_once('.')
        else
        {
            return (file_type, creator);
        };
        self.entries
            .iter()
            .find(|e| e.extension[..e.length].eq_ignore_ascii_case(extension.as_bytes()))
            .map_or((file_type, creator), |e| (e.file_type, e.creator))
    }
}
#[cfg(test)]
mod tests
{
    use super::*;
    #[test]
    fn preferences_classify_untyped_names_without_overriding_explicit_metadata()
    {
        let registry = Registry::decode(DEFAULT_JSON).unwrap();
        assert_eq!(registry.classify("Archive.SiT", [0; 4], [0; 4]), (*b"SIT!", *b"SIT!"));
        assert_eq!(registry.classify("Archive.sitx", *b"BINA", *b"????"), (*b"BINA", *b"????"));
        assert_eq!(registry.classify("Notes.sit", *b"TEXT", *b"EDIT"), (*b"TEXT", *b"EDIT"));
        let empty = Registry::decode(br#"{"version":1,"associations":[]}"#).unwrap();
        assert_eq!(empty.classify("Archive.sit", [0; 4], [0; 4]), ([0; 4], [0; 4]));
        let escaped = Registry::decode(br#"{"version":1,"associations":[{"extension":"\u0073it","file_type":"SIT!","creator":"SIT!"}]}"#).unwrap();
        assert_eq!(escaped.classify("Archive.sit", [0; 4], [0; 4]), (*b"SIT!", *b"SIT!"));
        assert!(matches!(Registry::decode(br#"{"version":2,"associations":[]}"#),
                         Err(Error::Version)));
        assert!(Registry::decode(br#"{"version":1,"associations":[{"extension":".sit","file_type":"SIT!","creator":"SIT!"}]}"#).is_err());
        assert!(matches!(Registry::decode(br#"{"version":1,"associations":[{"extension":"sit","file_type":"SIT!","creator":"SIT!"},{"extension":"SIT","file_type":"TEXT","creator":"????"}]}"#),Err(Error::Duplicate)));
    }
}
