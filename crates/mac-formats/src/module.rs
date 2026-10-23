//! Native module package validation, shared by mac-build, inspection and System.
//! Merely parsing this data never invokes code or authorizes loading.
#![forbid(unsafe_code)]
use mac_module::{Contract, PackageKind};
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
    pub fn inspect(resources: &'a [Resource], file_type: [u8; 4]) -> Result<Self>
    {
        let kind = match file_type
        {
            v if v == *b"DRVR" => PackageKind::Driver,
            v if v == *b"INIT" => PackageKind::Extension,
            _ => return Err(Error::Malformed("package is not a native driver or extension"))
        };
        if resources.iter().any(|r| [*b"NABI", *b"RV64", *b"RLOC"].contains(&r.kind))
        {
            return Err(Error::Malformed("module contains application code framing; rebuild the \
                                         package"));
        }
        let mut metadata = resources.iter().filter(|r| r.kind == *b"DABI" || r.kind == *b"EABI");
        let metadata = match (metadata.next(), metadata.next())
        {
            (Some(value), None)
                if value.kind == kind.metadata_type()
                   && value.id == 0
                   && value.attributes & 1 == 0 =>
            {
                value
            }
            _ =>
            {
                return Err(Error::Malformed("module requires exactly one matching DABI/EABI 0 \
                                             resource"));
            }
        };
        let contract = Contract::decode(&metadata.data).map_err(|_| {
                                                           Error::Malformed("invalid module \
                                                                             contract; rebuild \
                                                                             the package")
                                                       })?;
        if contract.kind != kind || contract.kernel_abi != crate::native::ABI_VERSION
        {
            return Err(Error::Malformed("module kind or kernel ABI mismatch; rebuild the \
                                         package"));
        }
        let mut entries = resources.iter().filter(|r| r.kind == contract.code_type);
        let entry = match (entries.next(), entries.next())
        {
            (Some(value), None) if value.id == contract.code_id && value.attributes & 1 == 0 =>
            {
                value
            }
            _ =>
            {
                return Err(Error::Malformed("module requires exactly one declared native entry \
                                             resource"));
            }
        };
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
