//! Native kernel modules: generic admission/lifetime plus narrow executable ABI.
pub mod devices;
pub mod file_sources;
pub mod jobs;
pub(crate) mod registry;
pub use registry::*;
#[cfg(target_arch = "riscv64")]
mod runtime;
#[cfg(target_arch = "riscv64")]
pub(crate) use runtime::*;
#[cfg(target_arch = "riscv64")]
pub use runtime::{OwnedJob, submit_job};

pub(crate) fn status(error: Error) -> mac_abi::Status
{
    use mac_abi::*;
    match error
    {
        Error::Stale => NOT_FOUND,
        Error::Identity(_) => NO_MEMORY,
        Error::Owner(error) => crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(error)),
        Error::Domain(mac_object_table::domain::DirectoryError::Storage { .. }) => NO_MEMORY,
        Error::Domain(mac_object_table::domain::DirectoryError::NotFound { .. }) => NOT_FOUND,
        Error::Domain(_) => SERVICE_BUSY,
        Error::Access => ACCESS_DENIED,
        Error::Busy | Error::State | Error::DependencyInUse { .. } | Error::Cleanup(_) => SERVICE_BUSY,
        Error::Exhausted => NO_MEMORY,
        Error::Contract(_) => BAD_ABI,
        Error::Duplicate | Error::Exclusive => BAD_ARGUMENT
    }
}
