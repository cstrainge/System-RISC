//! Archive failures survive asynchronous driver calls without a thread-local
//! last-error buffer. Payload ranges retain the external version/method byte.
use crate::Status;

const BASE: Status = -65_536;
const VERSION: Status = BASE - 256;
const METHOD: Status = VERSION - 256;
const ENTRY_VERSION: Status = METHOD - 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveError
{
    Truncated,
    Signature,
    HeaderChecksum,
    ForkChecksum,
    Malformed,
    Limit,
    Encrypted,
    Version(u8),
    EntryVersion(u8),
    Method(u8)
}
impl ArchiveError
{
    pub const fn status(self) -> Status
    {
        match self
        {
            Self::Truncated => BASE,
            Self::Signature => BASE - 1,
            Self::HeaderChecksum => BASE - 2,
            Self::ForkChecksum => BASE - 3,
            Self::Malformed => BASE - 4,
            Self::Limit => BASE - 5,
            Self::Encrypted => BASE - 6,
            Self::Version(version) => VERSION - version as Status,
            Self::EntryVersion(version) => ENTRY_VERSION - version as Status,
            Self::Method(method) => METHOD - method as Status
        }
    }

    pub const fn from_status(status: Status) -> Option<Self>
    {
        match status
        {
            BASE => Some(Self::Truncated),
            s if s == BASE - 1 => Some(Self::Signature),
            s if s == BASE - 2 => Some(Self::HeaderChecksum),
            s if s == BASE - 3 => Some(Self::ForkChecksum),
            s if s == BASE - 4 => Some(Self::Malformed),
            s if s == BASE - 5 => Some(Self::Limit),
            s if s == BASE - 6 => Some(Self::Encrypted),
            s if s <= VERSION && s >= VERSION - 255 => Some(Self::Version((VERSION - s) as u8)),
            s if s <= METHOD && s >= METHOD - 255 => Some(Self::Method((METHOD - s) as u8)),
            s if s <= ENTRY_VERSION && s >= ENTRY_VERSION - 255 =>
            {
                Some(Self::EntryVersion((ENTRY_VERSION - s) as u8))
            }
            _ => None
        }
    }
}
impl core::fmt::Display for ArchiveError
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::Truncated =>
            {
                f.write_str("StuffIt archive is truncated; required header or fork bytes are \
                             missing")
            }
            Self::Signature =>
            {
                f.write_str("file does not have a supported StuffIt archive signature")
            }
            Self::HeaderChecksum =>
            {
                f.write_str("StuffIt archive header checksum does not match; the catalog is \
                             damaged")
            }
            Self::ForkChecksum =>
            {
                f.write_str("StuffIt fork checksum does not match; extracted bytes failed \
                             verification")
            }
            Self::Malformed =>
            {
                f.write_str("StuffIt archive has invalid lengths, hierarchy or compressed data")
            }
            Self::Limit =>
            {
                f.write_str("StuffIt archive exceeds the driver's catalog, nesting or decoder \
                             limits")
            }
            Self::Encrypted =>
            {
                f.write_str("StuffIt archive is encrypted; password-protected extraction is not \
                             supported")
            }
            Self::Version(version) =>
            {
                write!(f,
                       "StuffIt archive version {version} is not supported by the installed driver")
            }
            Self::EntryVersion(version) =>
            {
                write!(f,
                       "StuffIt entry header version {version} is not supported by the installed \
                        driver")
            }
            Self::Method(method) =>
            {
                write!(f,
                       "StuffIt compression method {method} is not supported by the installed \
                        driver")
            }
        }
    }
}
