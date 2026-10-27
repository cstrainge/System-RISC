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
                if resources.iter().any(|r| [*b"NABI", *b"RV64", *b"RLOC", *b"INIT", *b"EABI"].contains(&r.kind))
                {
                    return Err(Error::Malformed("driver contains another executable framing; rebuild the DRVR package"));
                }
                let metadata =
                    unique(resources, *b"DABI", 0,
                           "driver requires exactly one DABI 0 contract; rebuild the package")?;
                let contract = decode_contract(metadata, PackageKind::Driver)?;
                let entry =
                    unique(resources, contract.code_type, contract.code_id,
                           "driver requires exactly one declared DRVR entry; rebuild the package")?;
                if resources.iter().filter(|r| r.kind == *b"DABI").count() != 1
                   || resources.iter().filter(|r| r.kind == *b"DRVR").count() != 1
                {
                    return Err(Error::Malformed("driver contains undeclared native entries or \
                                                 contracts; rebuild the package"));
                }
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
        validate_extension_structure(resources)?;
        let missing = match target
        {
            ExtensionTarget::System =>
            {
                "System extension requires RV64 0 and EABI 0; Finder-only code is not a System \
                 initializer; rebuild the System part"
            }
            ExtensionTarget::Finder =>
            {
                "Finder extension requires RV64 1 and EABI 1; System code is not a Finder \
                 initializer; rebuild the Finder part"
            }
        };
        let metadata = unique(resources, *b"EABI", target.resource_id(), missing)?;
        let contract = decode_contract(metadata, PackageKind::Extension)?;
        if contract.code_id != target.resource_id()
        {
            return Err(Error::Malformed("extension contract selects the wrong execution \
                                         target; rebuild the corresponding System or Finder \
                                         part"));
        }
        let entry =
            unique(resources, ExtensionTarget::RESOURCE_TYPE, target.resource_id(), missing)?;
        Self::admit(contract, entry)
    }

    fn admit(contract: Contract<'a>, entry: &Resource) -> Result<Self>
    {
        if entry.data.len() > mac_module::MAX_CODE_BYTES
        {
            return Err(Error::BudgetExceeded("module stored code exceeds the 2 MiB loading \
                                              budget"));
        }
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
        if contract.heap_bytes > mac_module::MAX_HEAP_BYTES
        {
            return Err(Error::BudgetExceeded("module declared heap exceeds the 4 MiB admission \
                                              budget"));
        }
        if contract.stack_bytes > mac_module::MAX_STACK_BYTES
        {
            return Err(Error::BudgetExceeded("module callback stack exceeds the 64 KiB \
                                              admission budget"));
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

fn decode_contract(metadata: &Resource, kind: PackageKind) -> Result<Contract<'_>>
{
    let contract = Contract::decode(&metadata.data).map_err(|_| {
                                                       Error::Malformed("invalid module contract; \
                                                                         rebuild the package")
                                                   })?;
    if contract.kind != kind || contract.kernel_abi != crate::native::ABI_VERSION
    {
        return Err(Error::Malformed("module kind or kernel ABI mismatch; rebuild the package"));
    }
    Ok(contract)
}

fn validate_extension_structure(resources: &[Resource]) -> Result<()>
{
    if resources.iter()
                .any(|r| [*b"NABI", *b"RLOC", *b"DRVR", *b"DABI", *b"INIT"].contains(&r.kind))
    {
        return Err(Error::Malformed("extension requires RV64 target envelopes, not \
                                     INIT/driver/application framing; rebuild the extension"));
    }
    if resources.iter().any(|r| {
                           (r.kind == ExtensionTarget::RESOURCE_TYPE || r.kind == *b"EABI")
                           && (ExtensionTarget::from_resource_id(r.id).is_none()
                               || r.attributes & 1 != 0)
                       })
    {
        return Err(Error::Malformed("extension code and EABI targets must be uncompressed IDs \
                                     0 (System) or 1 (Finder); rebuild the extension"));
    }
    for target in ExtensionTarget::ALL
    {
        let id = target.resource_id();
        let code = resources.iter()
                            .filter(|r| r.kind == ExtensionTarget::RESOURCE_TYPE && r.id == id)
                            .count();
        let metadata = resources.iter().filter(|r| r.kind == *b"EABI" && r.id == id).count();
        if code != metadata || code > 1
        {
            return Err(Error::Malformed("extension target requires one RV64 entry paired with \
                                         one EABI of the same ID; rebuild the incomplete or \
                                         duplicate part"));
        }
    }
    Ok(())
}
