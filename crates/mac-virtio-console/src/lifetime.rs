//! The cleanup walk never forgets a pending reset ticket or releases DMA before
//! acknowledgement. It is independent of MMIO so unhappy paths run on the host.
#![forbid(unsafe_code)]
use mac_driver::{Completion, Phase, Result};

pub(crate) trait Authority
{
    fn reset(&mut self, claim: u64, ticket: &mut u64, phase: Phase) -> Result<Completion>;
    fn unmap(&mut self, mapping: u64) -> Result<()>;
    fn release(&mut self, claim: u64) -> Result<()>;
}
pub(crate) struct Lifetime
{
    pub claim:          u64,
    pub mapping:        u64,
    pub ticket:         u64,
    pub reset_complete: bool
}
impl Lifetime
{
    pub(crate) const fn new(claim: u64) -> Self
    {
        Self { claim,
               mapping: 0,
               ticket: 0,
               reset_complete: false }
    }

    pub(crate) fn reset(&mut self, authority: &mut impl Authority) -> Result<bool>
    {
        if self.claim == 0 || self.reset_complete
        {
            return Ok(true);
        }
        let phase = if self.ticket == 0 { Phase::Start } else { Phase::Poll };
        match authority.reset(self.claim, &mut self.ticket, phase)?
        {
            Completion::Complete(0) if self.ticket == 0 =>
            {
                self.reset_complete = true;
                Ok(true)
            }
            Completion::Pending if self.ticket != 0 => Ok(false),
            Completion::Complete(_) | Completion::Pending =>
            {
                Err(mac_driver::Error(mac_abi::BAD_ABI))
            }
        }
    }

    pub(crate) fn cleanup(&mut self, authority: &mut impl Authority) -> Result<bool>
    {
        if !self.reset(authority)?
        {
            return Ok(false);
        }
        if self.mapping != 0
        {
            authority.unmap(self.mapping)?;
            self.mapping = 0;
        }
        if self.claim != 0
        {
            authority.release(self.claim)?;
            self.claim = 0;
        }
        Ok(true)
    }
}

