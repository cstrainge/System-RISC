//! Checked fixed stack policy, shared by platform builds and native admission.
//! It selects sizes; physical backing and mapped guard pages belong to the
//! architecture. Reserving a size does not create or activate a stack.
use core::fmt;

use crate::{pages::PageGeometry, policy::ConfiguredRange};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    Policy
    {
        minimum: u64, default: u64, maximum: u64, page_bytes: u64
    },
    Selection
    {
        requested: u64, minimum: u64, maximum: u64, page_bytes: u64
    },
    Context
    {
        cpu_bytes: u64, stack_bytes: u64
    },
    CpuStateOverflow
    {
        fixed_bytes:           u64,
        vector_register_count: u64,
        vector_register_bytes: u64
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Policy { minimum,
                           default,
                           maximum,
                           page_bytes } =>
            {
                write!(f,
                       "stack sizes must be nonzero and page aligned ({page_bytes} bytes), with \
                        minimum <= default <= maximum; configured {minimum}/{default}/{maximum}")
            }
            Self::Selection { requested,
                              minimum,
                              maximum,
                              page_bytes } =>
            {
                write!(f,
                       "stack request {requested} bytes is outside {minimum}..={maximum} or is \
                        not page aligned ({page_bytes} bytes)")
            }
            Self::Context { cpu_bytes,
                            stack_bytes } =>
            {
                write!(f,
                       "CPU state requires {cpu_bytes} bytes, leaving no execution space in the \
                        selected {stack_bytes}-byte stack; increase the stack policy or select a \
                        compatible CPU")
            }
            Self::CpuStateOverflow { fixed_bytes,
                                     vector_register_count,
                                     vector_register_bytes } =>
            {
                write!(f,
                       "CPU state size overflows: fixed header {fixed_bytes} bytes plus \
                        {vector_register_count} vector registers of {vector_register_bytes} bytes \
                        each")
            }
        }
    }
}
impl core::error::Error for Error
{
}

#[derive(Clone, Copy, Debug)]
pub struct Policy
{
    range:    ConfiguredRange,
    geometry: PageGeometry
}
impl Policy
{
    pub fn new(minimum: u64, default: u64, maximum: u64, geometry: PageGeometry)
               -> Result<Self, Error>
    {
        let invalid = || {
            Error::Policy { minimum,
                            default,
                            maximum,
                            page_bytes: geometry.bytes() }
        };
        let range = ConfiguredRange::new(minimum, default, maximum).map_err(|_| invalid())?;
        if minimum == 0 || [minimum, default, maximum].iter().any(|n| !geometry.aligned(*n))
        {
            return Err(invalid());
        }
        Ok(Self { range,
                  geometry })
    }

    /// Per-process requests use the same policy as the build-selected default.
    /// A refusal never rounds or silently substitutes another requested size.
    pub fn select(self, requested: Option<u64>, cpu_bytes: u64) -> Result<u64, Error>
    {
        let bytes = requested.unwrap_or(self.range.default_value());
        if !self.range.contains(bytes) || !self.geometry.aligned(bytes)
        {
            return Err(Error::Selection { requested:  bytes,
                                          minimum:    self.range.minimum(),
                                          maximum:    self.range.maximum(),
                                          page_bytes: self.geometry.bytes() });
        }
        if cpu_bytes >= bytes
        {
            return Err(Error::Context { cpu_bytes,
                                        stack_bytes: bytes });
        }
        Ok(bytes)
    }
}
/// The architecture supplies its register count and discovered register width.
pub fn cpu_state_bytes(fixed_bytes: u64, vector_register_count: u64, vector_register_bytes: u64)
                       -> Result<u64, Error>
{
    vector_register_bytes.checked_mul(vector_register_count)
                         .and_then(|bytes| bytes.checked_add(fixed_bytes))
                         .ok_or(Error::CpuStateOverflow { fixed_bytes,
                                                          vector_register_count,
                                                          vector_register_bytes })
}
#[cfg(test)]
mod tests
{
    extern crate alloc;
    use alloc::string::ToString;

    use super::*;
    #[test]
    fn defaults_explicit_choices_and_cpu_refusals_keep_exact_causes()
    {
        let page = PageGeometry::new(4096).unwrap();
        let policy = Policy::new(64 << 10, 4 << 20, 8 << 20, page).unwrap();
        assert_eq!(policy.select(None, 4096).unwrap(), 4 << 20);
        for selected in [64 << 10, (4 << 20) + 4096, 8 << 20]
        {
            assert_eq!(policy.select(Some(selected), 1024).unwrap(), selected);
        }
        for requested in [0, 65535, 65537, (8 << 20) + 4096]
        {
            let error = policy.select(Some(requested), 1024).unwrap_err();
            assert_eq!(error,
                       Error::Selection { requested,
                                          minimum: 65536,
                                          maximum: 8388608,
                                          page_bytes: 4096 });
            assert!(error.to_string().contains(&requested.to_string()));
        }
        for cpu_bytes in [65536, 65537]
        {
            let error = policy.select(Some(65536), cpu_bytes).unwrap_err();
            assert_eq!(error,
                       Error::Context { cpu_bytes,
                                        stack_bytes: 65536 });
            assert!(error.to_string().contains("increase the stack policy"));
        }
        assert!(matches!(Policy::new(0, 4096, 8192, page), Err(Error::Policy { .. })));
        assert!(matches!(Policy::new(4096, 4097, 8192, page), Err(Error::Policy { .. })));
        assert!(matches!(Policy::new(4096, 16384, 8192, page), Err(Error::Policy { .. })));
        assert_eq!(cpu_state_bytes(576, 32, 32).unwrap(), 1600);
        assert!(matches!(cpu_state_bytes(576, 32, u64::MAX), Err(Error::CpuStateOverflow { .. })));
        assert!(matches!(cpu_state_bytes(u64::MAX, 32, 1), Err(Error::CpuStateOverflow { .. })));
    }
}
