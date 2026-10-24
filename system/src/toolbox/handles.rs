//! Central generation-tagged object table shared by System and host providers.
//! Keep memory's original high16 generation/low16 one-based slot wire contract.
#![forbid(unsafe_code)]
pub(crate) mod cleanup;
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
pub(crate) type Store<T, S = ()> = mac_object_table::store::Store<T, InvocationGate, S>;
pub(crate) type KindLease = mac_object_table::registry::KindLease<InvocationGate>;
pub(crate) type Registry<C> =
    mac_object_table::registry::Registry<C, InvocationGate, cleanup::Failure>;
pub(crate) use mac_object_table::domain::{CapacityPolicy, KindCode};

pub(crate) fn registry_status(error: &mac_object_table::registry::Error) -> mac_abi::Status
{
    use mac_abi::*;
    use mac_object_table::{domain::PoolError, registry::Error};
    match error
    {
        Error::Missing { .. }
        | Error::Stale { .. }
        | Error::IndexNotFound { .. }
        | Error::RegistrationNotFound { .. } => NOT_FOUND,
        Error::Foreign { .. } => ACCESS_DENIED,
        Error::Busy { .. } => SERVICE_BUSY,
        Error::SnapshotStale { .. } => SCAN_STALE,
        Error::RevisionExhausted => NOT_SUPPORTED,
        Error::Pool(PoolError::Storage { .. }
                    | PoolError::IdentityExhausted
                    | PoolError::RegistrationUsesExhausted { .. }) => NO_MEMORY,
        Error::Pool(PoolError::RegistrationUnavailable { .. } | PoolError::Stopping { .. }) =>
        {
            SERVICE_BUSY
        }
        Error::Pool(_)
        | Error::Label { .. }
        | Error::Duplicate { .. }
        | Error::ReservedNamespace { .. } => BAD_ARGUMENT
    }
}

#[cfg(target_arch = "riscv64")]
#[derive(Debug)]
pub(crate) enum KindAccessError
{
    Owner(mac_object_table::domain::IndexError),
    Directory(DirectoryError),
    Registry(mac_object_table::registry::Error)
}
#[cfg(target_arch = "riscv64")]
impl core::fmt::Display for KindAccessError
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::Owner(error) => error.fmt(f),
            Self::Directory(error) => error.fmt(f),
            Self::Registry(error) => error.fmt(f)
        }
    }
}
#[cfg(target_arch = "riscv64")]
impl KindAccessError
{
    pub(crate) fn status(&self) -> mac_abi::Status
    {
        match self
        {
            Self::Owner(mac_object_table::domain::IndexError::Identity(_))
            | Self::Directory(DirectoryError::Storage { .. }) => mac_abi::NO_MEMORY,
            Self::Registry(error) => registry_status(error),
            Self::Owner(_) | Self::Directory(_) => mac_abi::BAD_ARGUMENT
        }
    }
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn registry<R>(run: impl FnOnce(&Registry<crate::toolbox::runtime::Desktop>) -> R)
                          -> Result<R, KindAccessError>
{
    use mac_object_table::shared::InvocationGate as _;
    type Resident = Registry<crate::toolbox::runtime::Desktop>;
    static REGISTRY: spin::Once<spinning_top::Spinlock<Resident>> = spin::Once::new();
    let registry = REGISTRY.try_call_once(|| {
                               let owner = system_owner_checked()?;
                               let mut registry = Resident::new();
                               registry.register(&owner,
                                         crate::toolbox::controls::objects::specification(),
                                         crate::toolbox::handles::cleanup::Complete(crate::toolbox::controls::objects::ReleaseControls))
                               .map_err(|error| {
                                   serial!("KIND_REGISTRATION_REFUSED: {error}");
                                   KindAccessError::Registry(error)
                               })?;
                               registry.register(&owner,
                                         crate::toolbox::controls::objects::storage_specification(),
                                         crate::toolbox::handles::cleanup::Complete(crate::toolbox::controls::objects::ReleaseStorage))
                               .map_err(KindAccessError::Registry)?;
                               registry.register(&owner,
                                         crate::toolbox::controls::scrollbox::specification(false),
                                         crate::toolbox::handles::cleanup::Complete(crate::toolbox::controls::scrollbox::ReleaseScrollBoxes))
                               .map_err(KindAccessError::Registry)?;
                               registry.register(&owner,
                                         crate::toolbox::controls::scrollbox::specification(true),
                                         crate::toolbox::handles::cleanup::Complete(crate::toolbox::controls::scrollbox::ReleaseStorage))
                               .map_err(KindAccessError::Registry)?;
                               registry.register(&owner,
                                         crate::toolbox::standard_file::specification(false),
                                         crate::toolbox::handles::cleanup::Complete(crate::toolbox::standard_file::ReleaseDialogs))
                               .map_err(KindAccessError::Registry)?;
                               registry.register(&owner,
                                         crate::toolbox::standard_file::specification(true),
                                         crate::toolbox::handles::cleanup::Complete(crate::toolbox::standard_file::ReleaseStorage))
                               .map_err(KindAccessError::Registry)?;
                               registry.register(&owner,
                                         crate::toolbox::quickdraw::worlds::specification(false),
                                         crate::toolbox::handles::cleanup::Complete(crate::toolbox::quickdraw::worlds::ReleaseWorlds))
                               .map_err(KindAccessError::Registry)?;
                               registry.register(&owner,
                                         crate::toolbox::quickdraw::worlds::specification(true),
                                         crate::toolbox::handles::cleanup::Complete(crate::toolbox::quickdraw::worlds::ReleaseStorage))
                               .map_err(KindAccessError::Registry)?;
                               registry.register(&owner, crate::toolbox::windows::lifetime::specification(false),
                                   crate::toolbox::windows::lifetime::ReleaseWindows).map_err(KindAccessError::Registry)?;
                               registry.register(&owner, crate::toolbox::windows::lifetime::specification(true),
                                   crate::toolbox::windows::lifetime::ReleaseStorage).map_err(KindAccessError::Registry)?;
                               registry.register(&owner, crate::toolbox::menus::lifetime::specification(false),
                                   cleanup::Complete(crate::toolbox::menus::lifetime::ReleaseMenus)).map_err(KindAccessError::Registry)?;
                               registry.register(&owner, crate::toolbox::menus::lifetime::specification(true),
                                   cleanup::Complete(crate::toolbox::menus::lifetime::ReleaseStorage)).map_err(KindAccessError::Registry)?;
                               registry.register(&owner, crate::toolbox::memory::lifetime::specification(false),
                                   crate::toolbox::memory::lifetime::ReleaseBlocks).map_err(KindAccessError::Registry)?;
                               registry.register(&owner, crate::toolbox::memory::lifetime::specification(true),
                                   crate::toolbox::memory::lifetime::ReleaseStorage).map_err(KindAccessError::Registry)?;
                               registry.register(&owner, crate::toolbox::textedit::lifetime::specification(false),
                                   cleanup::Complete(crate::toolbox::textedit::lifetime::ReleaseEditors)).map_err(KindAccessError::Registry)?;
                               registry.register(&owner, crate::toolbox::textedit::lifetime::specification(true),
                                   cleanup::Complete(crate::toolbox::textedit::lifetime::ReleaseStorage)).map_err(KindAccessError::Registry)?;
                               registry.register(&owner,crate::toolbox::dialogs::specification(false),crate::toolbox::dialogs::ReleaseAlerts).map_err(KindAccessError::Registry)?;
                               registry.register(&owner,crate::toolbox::dialogs::specification(true),crate::toolbox::dialogs::ReleaseStorage).map_err(KindAccessError::Registry)?;
                               use crate::toolbox::drag::lifetime::{Registration, ReleaseKind};
                               for kind in [Registration::Sessions, Registration::SessionStorage,
                                            Registration::Receivers, Registration::ReceiverStorage]
                               {
                                   registry.register(&owner, kind.specification(), cleanup::Complete(ReleaseKind(kind)))
                                           .map_err(KindAccessError::Registry)?;
                               }
                               Ok::<_, KindAccessError>(spinning_top::Spinlock::new(registry))
                           })?;
    Ok(InvocationGate::run(|| run(&registry.lock())))
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn controls_kinds() -> Result<(KindLease, KindLease), mac_abi::Status>
{
    registry(|registry| Ok((registry.get(KindCode(*b"CTRL"))?, registry.get(KindCode(*b"CSTA"))?))).map_err(|error| error.status())?
                                                         .map_err(|error| registry_status(&error))
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn scroll_box_kinds() -> Result<(KindLease, KindLease), mac_abi::Status>
{
    registry(|registry| Ok((registry.get(KindCode(*b"SBOX"))?, registry.get(KindCode(*b"SBST"))?)))
        .map_err(|error| error.status())?.map_err(|error| registry_status(&error))
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn file_dialog_kinds() -> Result<(KindLease, KindLease), mac_abi::Status>
{
    registry(|registry| Ok((registry.get(KindCode(*b"SFDL"))?, registry.get(KindCode(*b"SFST"))?)))
        .map_err(|error| error.status())?.map_err(|error| registry_status(&error))
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn system_owner() -> Result<OwnerRef, mac_abi::Status>
{
    system_owner_checked().map_err(|error| error.status())
}
#[cfg(target_arch = "riscv64")]
fn system_owner_checked() -> Result<OwnerRef, KindAccessError>
{
    static OWNER: spin::Once<OwnerRef> = spin::Once::new();
    OWNER.try_call_once(|| {
             let index = OwnerIndex::issue(DomainKind::System).map_err(KindAccessError::Owner)?;
             OwnerRef::try_new(index).map_err(KindAccessError::Directory)
         })
         .cloned()
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn world_kinds() -> Result<(KindLease, KindLease), mac_abi::Status>
{
    registry(|registry| Ok((registry.get(KindCode(*b"GWLD"))?, registry.get(KindCode(*b"GWST"))?)))
        .map_err(|error| error.status())?.map_err(|error| registry_status(&error))
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn window_kinds() -> Result<(KindLease, KindLease), mac_abi::Status>
{
    registry(|registry| Ok((registry.get(KindCode(*b"WIND"))?, registry.get(KindCode(*b"WNST"))?)))
        .map_err(|error| error.status())?.map_err(|error| registry_status(&error))
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn menu_kinds() -> Result<(KindLease, KindLease), mac_abi::Status>
{
    registry(|registry| Ok((registry.get(KindCode(*b"MENU"))?, registry.get(KindCode(*b"MNST"))?)))
        .map_err(|error| error.status())?.map_err(|error| registry_status(&error))
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn memory_kinds() -> Result<(KindLease, KindLease), mac_abi::Status>
{
    registry(|registry| Ok((registry.get(KindCode(*b"MMBL"))?, registry.get(KindCode(*b"MMST"))?)))
        .map_err(|error| error.status())?.map_err(|error| registry_status(&error))
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn editor_kinds() -> Result<(KindLease, KindLease), mac_abi::Status>
{
    registry(|registry| Ok((registry.get(KindCode(*b"TEDT"))?, registry.get(KindCode(*b"TESV"))?)))
        .map_err(|error| error.status())?.map_err(|error| registry_status(&error))
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn alert_kinds() -> Result<(KindLease, KindLease), mac_abi::Status>
{
    registry(|registry|Ok((registry.get(KindCode(*b"ALRT"))?,registry.get(KindCode(*b"ALST"))?)))
        .map_err(|error|error.status())?.map_err(|error|registry_status(&error))
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn drag_kinds(
    )
    -> Result<((KindLease, KindLease), (KindLease, KindLease)), mac_abi::Status>
{
    registry(|registry| {
        Ok(((registry.get(KindCode(*b"DRAG"))?, registry.get(KindCode(*b"DRST"))?),
            (registry.get(KindCode(*b"DRCV"))?, registry.get(KindCode(*b"DRVS"))?)))
    }).map_err(|error| error.status())?
      .map_err(|error| registry_status(&error))
}
