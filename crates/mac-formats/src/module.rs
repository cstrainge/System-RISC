//! Native module package validation, shared by mac-build, inspection and System.
//! Merely parsing this data never invokes code or authorizes loading.
#![forbid(unsafe_code)]
use mac_module::{Contract, ExtensionTarget, PackageKind};
use sha2::{Digest, Sha256};

use crate::{Error, Result,
            code::{CodeRegistry, NativeCode, NoTranslator},
            resource::Resource};

pub struct Package<'a>
{
    pub contract: Contract<'a>,
    pub code:     NativeCode
}

/// Complete-fork metadata, independent of payload projection. Supplying these
/// rows grants no execution authority and does not inspect unselected code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceHeader
{
    pub kind:       [u8; 4],
    pub id:         i16,
    pub attributes: u8
}
impl From<&Resource> for ResourceHeader
{
    fn from(resource: &Resource) -> Self
    {
        Self { kind:       resource.kind,
               id:         resource.id,
               attributes: resource.attributes }
    }
}
impl From<crate::resource::index::Entry<'_>> for ResourceHeader
{
    fn from(resource: crate::resource::index::Entry<'_>) -> Self
    {
        Self { kind:       resource.kind,
               id:         resource.id,
               attributes: resource.attributes }
    }
}

fn missing_part(target: ExtensionTarget) -> &'static str
{
    match target
    {
        ExtensionTarget::System =>
        {
            "System extension requires RV64 0 and EABI 0; Finder-only code is not a System \
             initializer; rebuild the System part"
        }
        ExtensionTarget::Finder =>
        {
            "Finder extension requires RV64 1 and EABI 1; System code is not a Finder initializer; \
             rebuild the Finder part"
        }
    }
}

/// Choose a required part after whole-index structural validation. This never
/// borrows another execution domain when the requested part is absent.
pub fn require_extension_target(target: ExtensionTarget,
                                rows: impl Iterator<Item = ResourceHeader>)
                                -> Result<()>
{
    if !rows.into_iter().any(|row| row.kind == *b"EABI" && row.id == target.resource_id())
    {
        return Err(Error::Malformed(missing_part(target)));
    }
    Ok(())
}

/// Validate all executable framing before selecting or allocating code. Both
/// full package inspection and resident seekable projection use these rules.
pub fn validate_index(file_type: [u8; 4], rows: impl Iterator<Item = ResourceHeader> + Clone)
                      -> Result<()>
{
    if file_type == *b"DRVR"
    {
        if rows.clone()
               .any(|row| [*b"NABI", *b"RV64", *b"RLOC", *b"INIT", *b"EABI"].contains(&row.kind))
        {
            return Err(Error::Malformed("driver contains another executable framing; rebuild \
                                         the DRVR package"));
        }
        let metadata = rows.clone().filter(|row| row.kind == *b"DABI");
        if metadata.clone().count() != 1
           || !metadata.clone().all(|row| row.id == 0 && row.attributes & 1 == 0)
        {
            return Err(Error::Malformed("driver requires exactly one uncompressed DABI 0 \
                                         contract; rebuild the package"));
        }
        let code = rows.filter(|row| row.kind == *b"DRVR");
        if code.clone().count() != 1 || !code.clone().all(|row| row.attributes & 1 == 0)
        {
            return Err(Error::Malformed("driver requires exactly one uncompressed declared \
                                         DRVR entry; rebuild the package"));
        }
        return Ok(());
    }
    if file_type != *b"INIT"
    {
        return Err(Error::Malformed("package is not a native driver or extension"));
    }
    if rows.clone()
           .any(|row| [*b"NABI", *b"RLOC", *b"DRVR", *b"DABI", *b"INIT"].contains(&row.kind))
    {
        return Err(Error::Malformed("extension requires RV64 target envelopes, not \
                                     INIT/driver/application framing; rebuild the extension"));
    }
    if rows.clone().any(|row| {
                       [*b"RV64", *b"EABI"].contains(&row.kind)
                       && (ExtensionTarget::from_resource_id(row.id).is_none()
                           || row.attributes & 1 != 0)
                   })
    {
        return Err(Error::Malformed("extension code and EABI targets must be uncompressed IDs \
                                     0 (System) or 1 (Finder); rebuild the extension"));
    }
    for target in ExtensionTarget::ALL
    {
        let code = rows.clone()
                       .filter(|row| row.kind == *b"RV64" && row.id == target.resource_id())
                       .count();
        let metadata = rows.clone()
                           .filter(|row| row.kind == *b"EABI" && row.id == target.resource_id())
                           .count();
        if code != metadata || code > 1
        {
            return Err(Error::Malformed("extension target requires one RV64 entry paired with \
                                         one EABI of the same ID; rebuild the incomplete or \
                                         duplicate part"));
        }
    }
    Ok(())
}

/// Read a length word first in a seekable consumer. This bound applies to every
/// declared native target, including a Finder body left unselected on disk.
pub fn validate_code_size(kind: [u8; 4], bytes: usize) -> Result<()>
{
    if [*b"RV64", *b"DRVR"].contains(&kind) && bytes > mac_module::MAX_CODE_BYTES
    {
        return Err(Error::BudgetExceeded("module stored code exceeds the 2 MiB loading budget; \
                                          rebuild the package"));
    }
    Ok(())
}

/// Apply before allocating or reading a contract in a seekable consumer.
pub fn validate_contract_size(bytes: usize) -> Result<()>
{
    if bytes > mac_module::MAX_BYTES
    {
        return Err(Error::BudgetExceeded("module contract exceeds the 16 KiB metadata loading \
                                          budget; rebuild the package"));
    }
    Ok(())
}

/// Validate bounded metadata for each present target before selected code is
/// admitted. This checks no code digest, envelope or relocation body.
pub fn validate_contract<'a>(data: &'a [u8], metadata: ResourceHeader, file_type: [u8; 4],
                             rows: impl Iterator<Item = ResourceHeader>)
                             -> Result<Contract<'a>>
{
    validate_contract_size(data.len())?;
    let expected = match file_type
    {
        v if v == *b"DRVR" && metadata.kind == *b"DABI" && metadata.id == 0 => PackageKind::Driver,
        v if v == *b"INIT"
             && metadata.kind == *b"EABI"
             && ExtensionTarget::from_resource_id(metadata.id).is_some() =>
        {
            PackageKind::Extension
        }
        _ =>
        {
            return Err(Error::Malformed("module contract has invalid package or target \
                                         framing; rebuild the package"));
        }
    };
    if metadata.attributes & 1 != 0
    {
        return Err(Error::Malformed("module contract metadata must be uncompressed; rebuild \
                                     the package"));
    }
    let contract = Contract::decode(data).map_err(|_| {
                                             Error::Malformed("invalid System or Finder module \
                                                               contract metadata; rebuild the \
                                                               corresponding package part")
                                         })?;
    if contract.kind != expected || contract.kernel_abi != crate::native::ABI_VERSION
    {
        return Err(Error::Malformed("System or Finder module kind or kernel ABI mismatch; \
                                     rebuild the corresponding package part"));
    }
    if contract.heap_bytes > mac_module::MAX_HEAP_BYTES
    {
        return Err(Error::BudgetExceeded("module declared heap exceeds the 4 MiB admission \
                                          budget; rebuild the corresponding package part"));
    }
    if contract.stack_bytes > mac_module::MAX_STACK_BYTES
    {
        return Err(Error::BudgetExceeded("module callback stack exceeds the 64 KiB admission \
                                          budget; rebuild the corresponding package part"));
    }
    if expected == PackageKind::Extension && contract.code_id != metadata.id
    {
        return Err(Error::Malformed("extension contract selects the wrong execution target; \
                                     rebuild the corresponding System or Finder part"));
    }
    if !rows.into_iter().any(|code| code.kind == contract.code_type && code.id == contract.code_id)
    {
        return Err(Error::Malformed("module contract has no matching declared code entry; \
                                     rebuild the package"));
    }
    Ok(contract)
}

fn headers(resources: &[Resource]) -> impl Iterator<Item = ResourceHeader> + Clone
{
    resources.iter().map(ResourceHeader::from)
}

fn preflight(resources: &[Resource], file_type: [u8; 4], target: Option<ExtensionTarget>)
             -> Result<Contract<'_>>
{
    validate_index(file_type, headers(resources))?;
    if let Some(target) = target
    {
        require_extension_target(target, headers(resources))?;
    }
    let id = target.map_or(0, ExtensionTarget::resource_id);
    let mut selected = None;
    for resource in resources
    {
        validate_code_size(resource.kind, resource.data.len())?;
        if [*b"DABI", *b"EABI"].contains(&resource.kind)
        {
            let contract =
                validate_contract(&resource.data, resource.into(), file_type, headers(resources))?;
            if resource.id == id
            {
                selected = Some(contract);
            }
        }
    }
    selected.ok_or(Error::Malformed("module preflight has no selected contract; rebuild the \
                                     package"))
}
impl<'a> Package<'a>
{
    /// Resident module admission selects only System extension code. A Finder
    /// resource can be inspected explicitly, but can never become a fallback
    /// kernel initializer because a System part is absent.
    pub fn inspect(resources: &'a [Resource], file_type: [u8; 4]) -> Result<Self>
    {
        match file_type
        {
            v if v == *b"DRVR" =>
            {
                let contract = preflight(resources, file_type, None)?;
                let entry =
                    unique(resources, contract.code_type, contract.code_id,
                           "driver requires exactly one declared DRVR entry; rebuild the package")?;
                Self::admit(contract, entry)
            }
            v if v == *b"INIT" => Self::inspect_extension(resources, ExtensionTarget::System),
            _ => Err(Error::Malformed("package is not a native driver or extension"))
        }
    }

    /// Validate a declared extension part without choosing its execution domain
    /// from its optional resource name. Finder execution is a separate consumer;
    /// inspecting it does not grant kernel-module calling authority.
    pub fn inspect_extension(resources: &'a [Resource], target: ExtensionTarget) -> Result<Self>
    {
        let contract = preflight(resources, *b"INIT", Some(target))?;
        let entry = unique(resources,
                           ExtensionTarget::RESOURCE_TYPE,
                           target.resource_id(),
                           missing_part(target))?;
        Self::admit(contract, entry)
    }

    fn admit(contract: Contract<'a>, entry: &Resource) -> Result<Self>
    {
        if !entry.data.starts_with(b"RVN1")
        {
            return Err(Error::Malformed("module entry is not native code"));
        }
        let digest: [u8; 32] = Sha256::digest(&entry.data).into();
        if digest != contract.code_digest
        {
            return Err(Error::Malformed("module code does not match its contract digest"));
        }
        let code = CodeRegistry::new().prepare(entry, 0, &mut NoTranslator)?;
        if code.descriptor.load_base & 4095 != 0 || code.descriptor.load_limit & 4095 != 0
        {
            return Err(Error::Malformed("module load reservation is not page aligned"));
        }
        if code.descriptor.load_limit - code.descriptor.load_base > mac_module::MAX_LOAD_BYTES
        {
            return Err(Error::BudgetExceeded("module runtime reservation exceeds the 6 MiB \
                                              loading budget"));
        }
        Ok(Self { contract,
                  code })
    }
}

fn unique<'a>(resources: &'a [Resource], kind: [u8; 4], id: i16, missing: &'static str)
              -> Result<&'a Resource>
{
    let mut entries = resources.iter().filter(|r| r.kind == kind && r.id == id);
    match (entries.next(), entries.next())
    {
        (Some(value), None) if value.attributes & 1 == 0 => Ok(value),
        _ => Err(Error::Malformed(missing))
    }
}
