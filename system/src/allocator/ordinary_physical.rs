//! Refundable System-owned raw page payload. This is not fixed-heap occupancy,
//! Foundation storage or a synthetic AccountUse. The factory requires genuine
//! reset-lifetime System resource authority and owns a newly acquired lease.
#![forbid(unsafe_code)]
use core::{fmt, ops::Deref};

use crate::{kernel::SystemResources,
            machine::pages::{self, Backing}};
#[derive(Debug)]
pub(crate) enum Error
{
    Pages(pages::Error),
    Stopping,
    CounterOverflow
    {
        used:      u64,
        requested: u64
    }
}
impl Error
{
    pub(crate) fn status(&self) -> mac_abi::Status
    {
        match self
        {
            Self::Pages(e) => e.status(),
            Self::Stopping => mac_abi::SERVICE_BUSY,
            Self::CounterOverflow { .. } => mac_abi::NOT_SUPPORTED
        }
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Pages(e) => e.fmt(f),
            Self::Stopping => f.write_str("System physical source stopped before admission"),
            Self::CounterOverflow { used,
                                    requested } =>
            {
                write!(f,
                       "ordinary System physical accounting cannot represent {requested} bytes \
                        after {used}; backing unpublished")
            }
        }
    }
}
pub(crate) struct SystemBacking
{
    backing:  Backing,
    _receipt: super::budget::OrdinaryPhysicalReceipt
}
impl SystemBacking
{
    pub(crate) fn reserve_uninitialized(source: SystemResources, bytes: usize)
                                        -> Result<Self, Error>
    {
        // The sealed source proves actual System ownership. The page allocator
        // supplies the only storage admission limit; this creates no new quota.
        if !source.owner().accepting()
        {
            return Err(Error::Stopping);
        }
        let backing = Backing::reserve_uninitialized(bytes).map_err(Error::Pages)?;
        let receipt = super::budget::reserve_ordinary_physical(backing.bytes())?;
        Ok(Self { backing,
                  _receipt: receipt })
    }
}
impl Deref for SystemBacking
{
    type Target = Backing;

    fn deref(&self) -> &Backing
    {
        &self.backing
    }
}
