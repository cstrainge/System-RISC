//! Platform-neutral owned allocation selection. Account authority is genuine
//! and retained; an observed domain or execution label cannot manufacture it.
#![forbid(unsafe_code)]
use mac_kernel_model::{account::{AccountUse, OperationAccountUse},
                       policy::MemoryClass};
use mac_object_table::{domain::DomainId,
                       scope::{AllocationAdmission, Failure, Lease}};

use crate::toolbox::handles::InvocationGate;

#[derive(Clone, Copy)]
pub(crate) enum SystemClass
{
    Foundation,
    Ordinary
}

#[derive(Clone)]
pub(crate) enum Target
{
    System(SystemClass),
    Operation(OperationTarget),
    Owner
    {
        account: AccountUse<InvocationGate>,
        class:   MemoryClass
    }
}

#[derive(Clone)]
pub(crate) enum OperationTarget
{
    System
    {
        class: SystemClass, admission: AllocationAdmission<InvocationGate>
    },
    Owner
    {
        account: OperationAccountUse<InvocationGate>, class: MemoryClass
    }
}
impl Target
{
    /// Observational identity for comparing already-authentic capabilities.
    /// This cannot manufacture an account or an operation permit.
    pub(crate) fn owner(&self) -> DomainId
    {
        match self
        {
            Self::System(_) | Self::Operation(OperationTarget::System { .. }) => DomainId::SYSTEM,
            Self::Owner { account,
                          .. } => account.owner(),
            Self::Operation(OperationTarget::Owner { account,
                                                     .. }) => account.owner()
        }
    }

    /// Bind the original account/class to the actual published resource root.
    /// Minting validates the full live/pending key under its owner ledger once;
    /// subsequent allocations use only the independent scope/account guards.
    pub(crate) fn in_scope(&self, scope: &Lease<InvocationGate>)
                           -> Result<Self, mac_kernel_model::account::Error>
    {
        use mac_kernel_model::account::Error;
        if self.owner() != scope.subject()
        {
            return Err(Error::Operation(Failure::ForeignOwner { expected: self.owner(),
                                                                actual:   scope.subject() }));
        }
        let operation = match self
        {
            Self::Owner { account,
                          class } =>
            {
                OperationTarget::Owner { account: account.for_operation(scope)?,
                                         class:   *class }
            }
            Self::Operation(OperationTarget::Owner { account,
                                                     class }) =>
            {
                OperationTarget::Owner { account: account.for_operation(scope)?,
                                         class:   *class }
            }
            Self::System(class) =>
            {
                OperationTarget::System { class:     *class,
                                          admission: scope.allocation_admission()
                                                          .map_err(Error::Operation)? }
            }
            Self::Operation(OperationTarget::System { class,
                                                      admission }) =>
            {
                let admission = if admission.same_scope(scope)
                {
                    admission.with_admission(|| ()).map_err(Error::Operation)?;
                    admission.clone()
                }
                else
                {
                    scope.allocation_admission().map_err(Error::Operation)?
                };
                OperationTarget::System { class: *class,
                                          admission }
            }
        };
        Ok(Self::Operation(operation))
    }
}

pub(crate) fn account_status(error: &mac_kernel_model::account::Error) -> mac_abi::Status
{
    use mac_abi::*;
    use mac_kernel_model::account::Error;
    match error
    {
        Error::Closed { .. } => SERVICE_BUSY,
        Error::ForeignAccount { .. } => ACCESS_DENIED,
        Error::Budget(_) | Error::Metadata { .. } => NO_MEMORY,
        Error::Operation(cause) =>
        {
            match cause
            {
                Failure::ForeignOwner { .. } => ACCESS_DENIED,
                Failure::RootGone { .. } => NOT_FOUND,
                Failure::Closed | Failure::Unpublished => SERVICE_BUSY,
                Failure::WrongKind { .. } => BAD_ARGUMENT
            }
        }
    }
}

/// Runtime allocation selection is architecture-owned. An unported architecture
/// cannot execute this operation with substitute allocation authority.
pub(crate) fn with_target<R>(target: Target, operation: impl FnOnce() -> R)
                             -> Result<R, mac_abi::Status>
{
    #[cfg(target_arch = "riscv64")]
    {
        Ok(crate::context::with_allocation(target, operation))
    }
    #[cfg(not(target_arch = "riscv64"))]
    {
        let _ = (target, operation);
        Err(mac_abi::NOT_SUPPORTED)
    }
}

pub(crate) fn with_kernel_allocation<R>(account: AccountUse<InvocationGate>,
                                        operation: impl FnOnce() -> R)
                                        -> Result<R, mac_abi::Status>
{
    with_target(Target::Owner { account,
                                class: MemoryClass::Kernel },
                operation)
}

pub(crate) fn capture_target() -> Result<Target, mac_abi::Status>
{
    #[cfg(target_arch = "riscv64")]
    {
        crate::context::capture_allocation()
    }
    #[cfg(not(target_arch = "riscv64"))]
    {
        Err(mac_abi::NOT_SUPPORTED)
    }
}
