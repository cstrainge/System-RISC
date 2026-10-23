//! Central generation-tagged object table shared by System and host providers.
//! Keep memory's original high16 generation/low16 one-based slot wire contract.
#![forbid(unsafe_code)]
#[cfg(target_arch = "riscv64")]
pub(crate) use mac_object_table::domain::{DirectoryError, DomainKind, OwnerIndex};
#[cfg(target_arch = "riscv64")]
pub(crate) type Directory<T> =
    mac_object_table::domain::Directory<T, crate::machine::interrupts::NativeInvocationGate>;
pub(crate) use mac_object_table::{Kind, Table};

#[cfg(target_arch = "riscv64")]
pub(crate) type InvocationGate = crate::machine::interrupts::NativeInvocationGate;
#[cfg(not(target_arch = "riscv64"))]
pub(crate) struct InvocationGate;
#[cfg(not(target_arch = "riscv64"))]
impl mac_object_table::shared::InvocationGate for InvocationGate
{
    fn run<R>(operation: impl FnOnce() -> R) -> R
    {
        operation()
    }
}
pub(crate) type OwnerRef = mac_object_table::shared::OwnerRef<InvocationGate>;
pub(crate) type Store<T> = mac_object_table::store::Store<T, InvocationGate>;
pub(crate) use mac_object_table::domain::{CapacityPolicy, KindCode};

#[cfg(target_arch = "riscv64")]
pub(crate) fn system_owner() -> Result<OwnerRef, mac_abi::Status>
{
    static OWNER: spin::Once<OwnerRef> = spin::Once::new();
    OWNER.try_call_once(|| {
             let index = OwnerIndex::issue(DomainKind::System).map_err(|_| mac_abi::NO_MEMORY)?;
             OwnerRef::try_new(index).map_err(|_| mac_abi::NO_MEMORY)
         })
         .cloned()
}
