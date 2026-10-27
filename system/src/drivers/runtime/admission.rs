//! Error projection at module boot admission, before entry or publication.
#![forbid(unsafe_code)]

use mac_abi::{BAD_ABI, BAD_ARGUMENT, NO_MEMORY, NOT_FOUND, Status};

#[cfg(target_arch = "riscv64")]
use crate::boot_screen::ExtensionRefusal;
#[cfg(not(target_arch = "riscv64"))]
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
        | mac_formats::Error::Malformed(_) => BAD_ABI
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

#[cfg(test)]
mod tests
{
    use super::*;
    use crate::testing::allocations::Observe;

    #[test]
    fn module_package_admission_preserves_exhaustion_identity_without_reading_prose()
    {
        use mac_formats::Error;
        // Identical prose cannot affect the classification; arbitrary caller
        // detail is retained by Error's Display for the native diagnostic.
        let cases = [(Error::BudgetExceeded("module admission refused"), NO_MEMORY),
                     (Error::AllocationFailed("module admission refused"), NO_MEMORY),
                     (Error::Missing("module admission refused"), BAD_ABI),
                     (Error::TranslationRequired("module admission refused"), BAD_ABI),
                     (Error::Malformed("module admission refused"), BAD_ABI)];
        let observed = Observe::begin(true, None);
        let actual = cases.map(|(cause, _)| package_status(cause));
        let allocations = observed.finish();
        assert_eq!(allocations, 0);
        assert_eq!(actual, cases.map(|(_, status)| status));
        for (cause, _) in cases
        {
            assert_eq!(cause.message(), "module admission refused");
        }
    }

    #[test]
    fn module_metadata_refusal_keeps_memory_and_storage_causes_out_of_the_contract_mark()
    {
        let observed = Observe::begin(true, None);
        let memory = metadata_refusal(NO_MEMORY);
        let io = metadata_refusal(mac_abi::IO_ERROR);
        let busy = metadata_refusal(mac_abi::SERVICE_BUSY);
        let access = metadata_refusal(mac_abi::ACCESS_DENIED);
        let unsupported = metadata_refusal(mac_abi::NOT_SUPPORTED);
        let unknown = metadata_refusal(-12345);
        let allocations = observed.finish();
        assert_eq!(allocations, 0);
        assert_eq!(memory.status, NO_MEMORY);
        assert_eq!(memory.refusal, ExtensionRefusal::Initialization(NO_MEMORY));
        assert!(memory.reason.contains("memory-admission-refused"));
        assert!(memory.action.contains("free System memory") && memory.action.contains("retry"));
        assert!(memory.refusal.cause().contains("storage could not be admitted"));
        for (failure, status) in [(io, mac_abi::IO_ERROR),
                                  (busy, mac_abi::SERVICE_BUSY),
                                  (access, mac_abi::ACCESS_DENIED),
                                  (unsupported, mac_abi::NOT_SUPPORTED),
                                  (unknown, -12345)]
        {
            assert_eq!(failure.status, status);
            assert_eq!(failure.refusal, ExtensionRefusal::Initialization(status));
            assert_eq!(failure.reason, "module-contract-read-refused");
            assert!(failure.action.contains("status") && failure.action.contains("retry"));
        }
    }

    #[test]
    fn missing_and_invalid_contracts_keep_the_existing_rebuild_refusal()
    {
        for status in [BAD_ABI, BAD_ARGUMENT, NOT_FOUND]
        {
            let failure = metadata_refusal(status);
            assert_eq!(failure.status, BAD_ABI);
            assert_eq!(failure.refusal, ExtensionRefusal::Contract);
            assert_eq!(failure.reason, "missing-or-invalid-contract");
            assert!(failure.action.contains("rebuild")
                    && failure.action.contains("System contract"));
            assert!(failure.refusal.cause().contains("missing or invalid"));
        }
    }
}
