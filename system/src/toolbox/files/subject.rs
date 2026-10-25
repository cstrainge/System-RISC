//! File-service subject authority. An operation's identity is its resource scope,
//! not an integer masquerading as a process or drawing from the System ledger.
#![forbid(unsafe_code)]
#[cfg(target_arch = "riscv64")]
use mac_abi::Status;
use mac_object_table::scope;

use crate::toolbox::handles::{InvocationGate, OwnerRef};

pub(super) type Lease = scope::Lease<InvocationGate>;
pub(super) type Root = scope::Reservation<InvocationGate>;

#[derive(Clone, Copy)]
pub(super) enum Access<'a>
{
    Client(u64),
    Operation(&'a Lease)
}
impl From<u64> for Access<'_>
{
    fn from(owner: u64) -> Self
    {
        Self::Client(owner)
    }
}
impl<'a> From<&'a Lease> for Access<'a>
{
    fn from(scope: &'a Lease) -> Self
    {
        Self::Operation(scope)
    }
}
impl Access<'_>
{
    pub(super) fn label(self) -> u64
    {
        match self
        {
            Self::Client(label) => label,
            Self::Operation(scope) => scope.subject().incarnation()
        }
    }

    pub(super) fn owned(self) -> Subject
    {
        match self
        {
            Self::Client(label) => Subject::Client(label),
            Self::Operation(scope) => Subject::Operation(scope.clone())
        }
    }

    #[cfg(target_arch = "riscv64")]
    pub(super) fn authority(self) -> Result<Authority, Status>
    {
        match self
        {
            Self::Client(label) => super::creation_authority(label).map(Authority::Client),
            Self::Operation(scope) =>
            {
                scope.admitting().map_err(|cause|crate::toolbox::handles::projection_status(
                    mac_object_table::projected::Error::Pool(mac_object_table::domain::PoolError::Scope {
                        owner:scope.subject(),kind:scope.kind(),cause})))?;
                Ok(Authority::Operation(scope.clone()))
            }
        }
    }
}

#[derive(Clone)]
pub(super) enum Subject
{
    Client(u64),
    Operation(Lease)
}
impl Subject
{
    pub(super) fn access(&self) -> Access<'_>
    {
        match self
        {
            Self::Client(label) => Access::Client(*label),
            Self::Operation(scope) => Access::Operation(scope)
        }
    }

    pub(super) fn label(&self) -> u64
    {
        self.access().label()
    }

    pub(super) fn matches(&self, other: Access<'_>) -> bool
    {
        match (self, other)
        {
            (Self::Client(left), Access::Client(right)) => *left == right,
            (Self::Operation(left), Access::Operation(right)) => left.same_scope(right),
            _ => false
        }
    }
}

#[derive(Clone)]
pub(super) enum Authority
{
    Client(OwnerRef),
    Operation(Lease)
}
impl Authority
{
    pub(super) fn flight_subject(&self, label: u64) -> Subject
    {
        match self
        {
            Self::Client(_) => Subject::Client(label),
            Self::Operation(scope) => Subject::Operation(scope.clone())
        }
    }

    pub(super) fn accepting(&self) -> bool
    {
        match self
        {
            Self::Client(owner) => owner.accepting(),
            Self::Operation(scope) => scope.subject_accepting()
        }
    }

    pub(super) fn scope(&self) -> Option<&Lease>
    {
        match self
        {
            Self::Client(_) => None,
            Self::Operation(scope) => Some(scope)
        }
    }

    pub(super) fn matches(&self, label: u64, access: Access<'_>) -> bool
    {
        match (self, access)
        {
            (Self::Client(_), Access::Client(other)) => label == other,
            (Self::Operation(left), Access::Operation(right)) => left.same_scope(right),
            _ => false
        }
    }
}

impl From<u64> for Subject
{
    fn from(owner: u64) -> Self
    {
        Self::Client(owner)
    }
}
impl PartialEq<u64> for Subject
{
    fn eq(&self, other: &u64) -> bool
    {
        self.matches(Access::Client(*other))
    }
}
