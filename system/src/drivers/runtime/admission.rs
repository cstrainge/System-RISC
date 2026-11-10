//! Error projection at module boot admission, before entry or publication.
#![forbid(unsafe_code)]

use mac_abi::{BAD_ABI, BAD_ARGUMENT, NO_MEMORY, NOT_FOUND, Status};

#[cfg(systemrisc)]
use crate::boot_screen::ExtensionRefusal;
#[cfg(not(systemrisc))]
use crate::testing::extension_startup::Refusal as ExtensionRefusal;

/// Module packages historically reject non-memory format failures as BAD_ABI.
/// Preserve that boundary without hiding a healthy package's admission refusal.
pub(super) fn package_status(cause: mac_formats::Error) -> Status
{
    match cause
    {
        mac_formats::Error::BudgetExceeded(_) | mac_formats::Error::AllocationFailed(_) =>
        {
            NO_MEMORY
        }
        mac_formats::Error::Missing(_)
        | mac_formats::Error::TranslationRequired(_)
        | mac_formats::Error::Malformed(_)
        | mac_formats::Error::TextConversion(_) => BAD_ABI
    }
}

pub(super) struct MetadataRefusal
{
    pub status:  Status,
    pub refusal: ExtensionRefusal,
    pub reason:  &'static str,
    pub action:  &'static str
}

/// A read/storage failure is not evidence that a module contract is invalid.
/// Only the existing invalid/missing contract statuses use that boot-strip mark.
pub(super) fn metadata_refusal(cause: Status) -> MetadataRefusal
{
    match cause
    {
        BAD_ABI | BAD_ARGUMENT | NOT_FOUND =>
        {
            MetadataRefusal { status:  BAD_ABI,
                              refusal: ExtensionRefusal::Contract,
                              reason:  "missing-or-invalid-contract",
                              action:  "rebuild the module with a valid System contract" }
        }
        NO_MEMORY =>
        {
            MetadataRefusal { status:  cause,
                              refusal: ExtensionRefusal::Initialization(cause),
                              reason:  "module-contract-memory-admission-refused",
                              action:  "free System memory and retry the unchanged module" }
        }
        _ =>
        {
            MetadataRefusal { status:  cause,
                              refusal: ExtensionRefusal::Initialization(cause),
                              reason:  "module-contract-read-refused",
                              action:  "check the reported storage or service status and retry" }
        }
    }
}
