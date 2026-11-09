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
