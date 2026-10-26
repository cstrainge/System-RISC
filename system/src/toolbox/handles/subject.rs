//! Owned request subjects shared by resident services. Numeric delivery labels
//! remain separate; these capabilities are the actual allocation authorities.
#![forbid(unsafe_code)]
use super::{InvocationGate, OwnerRef};

#[derive(Clone)]
pub(crate) enum Authority
{
    Client(OwnerRef),
    Operation(mac_object_table::scope::Lease<InvocationGate>)
}
