//! Resident admission checks are independent of any particular device class.
//! They run before module entry; Finder parts require their separate execution
//! consumer and can never enter this kernel policy through decoded metadata.
use crate::*;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Available
{
    pub module:  [u8; 16],
    pub class:   ClassId,
    pub version: u32
}
#[derive(Clone, Copy)]
pub struct Policy<'a>
{
    /// Determined from the selected blessed installation's discovery path.
    pub directory:  PackageKind,
    pub file_type:  [u8; 4],
    pub kernel_abi: u32,
    pub target:     Target,
    pub max_heap:   u32,
    pub max_stack:  u32,
    pub services:   &'a [Service],
    /// Only successfully initialized, published providers appear here.
    pub providers:  &'a [Available]
}
impl Policy<'_>
{
    pub fn validate(&self, contract: Contract<'_>) -> Result<()>
    {
        if self.directory != contract.kind
           || self.file_type != contract.kind.file_type()
           || contract.kind == PackageKind::Extension
              && contract.code_id != ExtensionTarget::System.resource_id()
        {
            return Err(Error::WrongLocation);
        }
        if self.kernel_abi != contract.kernel_abi || contract.broker_abi != BROKER_ABI
        {
            return Err(Error::Version);
        }
        if !self.target.supports(contract.target)
        {
            return Err(Error::Target);
        }
        if contract.heap_bytes > self.max_heap || contract.stack_bytes > self.max_stack
        {
            return Err(Error::Limit);
        }
        for required in contract.services()
        {
            let required = required?;
            if !required.optional
               && !self.services
                       .iter()
                       .any(|s| s.service == required.service && s.version == required.version)
            {
                return Err(Error::MissingService);
            }
        }
        for required in contract.dependencies()
        {
            let required = required?;
            if !required.optional
               && !self.providers.iter().any(|p| {
                                            (required.module == [0; 16]
                                             || p.module == required.module)
                                            && p.class == required.class
                                            && p.version == required.version
                                        })
            {
                return Err(Error::MissingDependency);
            }
        }
        Ok(())
    }
}
