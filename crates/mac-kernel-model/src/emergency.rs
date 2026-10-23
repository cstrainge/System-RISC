//! Manifest for the preallocated failure-path kit. No buffers are allocated by
//! this type; actual contents and their sizes land with heap bring-up.
use crate::policy::ModelError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmergencyService
{
    FaultReport,
    OutOfMemoryReport,
    LedgerTeardown,
    BootstrapWatchdog,
    SaveChanges,
    SaveFile,
    SaveAs,
    CoreAlert,
    FinderRestart,
    SystemPanicRestart
}
/// Requirements include records, pinned resources and scratch buffers. Counts
/// are measured/configured by the owning path, not invented by this manifest.
#[derive(Clone, Copy, Debug)]
pub struct Requirement
{
    pub service:   EmergencyService,
    pub bytes:     u64,
    pub alignment: u64
}
pub struct EmergencyManifest<'a>
{
    requirements: &'a [Requirement],
    bytes:        u64
}
impl<'a> EmergencyManifest<'a>
{
    pub fn new(requirements: &'a [Requirement]) -> Result<Self, ModelError>
    {
        let mut bytes = 0u64;
        for (i, requirement) in requirements.iter().enumerate()
        {
            if !requirement.alignment.is_power_of_two()
               || requirements[..i].iter().any(|r| r.service == requirement.service)
            {
                return Err(ModelError::InvalidManifest);
            }
            let pad =
                (requirement.alignment - bytes % requirement.alignment) % requirement.alignment;
            bytes = bytes.checked_add(pad)
                         .and_then(|n| n.checked_add(requirement.bytes))
                         .ok_or(ModelError::Overflow)?;
        }
        Ok(Self { requirements,
                  bytes })
    }

    /// Conservative arena alignment; the eventual arena start must satisfy it.
    pub fn alignment(&self) -> u64
    {
        self.requirements.iter().map(|r| r.alignment).max().unwrap_or(1)
    }

    pub const fn bytes(&self) -> u64
    {
        self.bytes
    }

    pub const fn requirements(&self) -> &[Requirement]
    {
        self.requirements
    }
}
#[cfg(test)]
mod tests
{
    use super::*;
    #[test]
    fn manifest_sizes_and_refuses_duplicate_or_overflow_before_installation()
    {
        let requirements = [Requirement { service:   EmergencyService::FaultReport,
                                          bytes:     9,
                                          alignment: 8 },
                            Requirement { service:   EmergencyService::SaveFile,
                                          bytes:     17,
                                          alignment: 16 }];
        let manifest = EmergencyManifest::new(&requirements).unwrap();
        assert_eq!(manifest.bytes(), 33);
        assert_eq!(manifest.alignment(), 16);
        assert!(matches!(EmergencyManifest::new(&[requirements[0], requirements[0]]),
                         Err(ModelError::InvalidManifest)));
        let overflow = [Requirement { bytes: u64::MAX,
                                      ..requirements[0] },
                        requirements[1]];
        assert!(matches!(EmergencyManifest::new(&overflow), Err(ModelError::Overflow)));
    }
}
