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

#[cfg(test)]
mod tests
{
    use mac_driver::Error;

    use super::*;
    struct Fake
    {
        calls:         std::vec::Vec<&'static str>,
        pending:       bool,
        unmap_fails:   bool,
        release_fails: bool
    }
    #[test]
    fn a_failed_reset_keeps_its_live_ticket_and_prevents_unmapping()
    {
        struct FailedReset;
        impl Authority for FailedReset
        {
            fn reset(&mut self, _: u64, ticket: &mut u64, _: Phase) -> Result<Completion>
            {
                *ticket = 97;
                Err(Error(mac_abi::IO_ERROR))
            }

            fn unmap(&mut self, _: u64) -> Result<()>
            {
                panic!("unmap before reset acknowledgement");
            }

            fn release(&mut self, _: u64) -> Result<()>
            {
                panic!("release before reset acknowledgement");
            }
        }
        let mut life = Lifetime::new(11);
        life.mapping = 13;
        assert_eq!(life.cleanup(&mut FailedReset), Err(Error(mac_abi::IO_ERROR)));
        assert_eq!((life.claim, life.mapping, life.ticket, life.reset_complete),
                   (11, 13, 97, false));
        assert!(crate::Diagnostic::Cleanup.message().contains("pinned storage is retained"));
    }
    impl Authority for Fake
    {
        fn reset(&mut self, _: u64, ticket: &mut u64, phase: Phase) -> Result<Completion>
        {
            self.calls.push(if phase == Phase::Start { "reset-start" } else { "reset-poll" });
            if self.pending
            {
                *ticket = 71;
                Ok(Completion::Pending)
            }
            else
            {
                *ticket = 0;
                Ok(Completion::Complete(0))
            }
        }

        fn unmap(&mut self, _: u64) -> Result<()>
        {
            self.calls.push("unmap");
            if self.unmap_fails { Err(Error(mac_abi::IO_ERROR)) } else { Ok(()) }
        }

        fn release(&mut self, _: u64) -> Result<()>
        {
            self.calls.push("release");
            if self.release_fails { Err(Error(mac_abi::IO_ERROR)) } else { Ok(()) }
        }
    }
    #[test]
    fn pending_and_failed_cleanup_retain_the_exact_owner_and_ticket()
    {
        let mut life = Lifetime::new(19);
        life.mapping = 23;
        let mut fake = Fake { calls:         std::vec::Vec::new(),
                              pending:       true,
                              unmap_fails:   false,
                              release_fails: false };
        assert_eq!(life.cleanup(&mut fake), Ok(false));
        assert_eq!((life.claim, life.mapping, life.ticket), (19, 23, 71));
        assert_eq!(fake.calls, ["reset-start"]);
        fake.pending = false;
        fake.unmap_fails = true;
        assert_eq!(life.cleanup(&mut fake), Err(Error(mac_abi::IO_ERROR)));
        assert_eq!((life.claim, life.mapping), (19, 23));
        fake.unmap_fails = false;
        fake.release_fails = true;
        assert_eq!(life.cleanup(&mut fake), Err(Error(mac_abi::IO_ERROR)));
        assert_eq!((life.claim, life.mapping), (19, 0));
        fake.release_fails = false;
        assert_eq!(life.cleanup(&mut fake), Ok(true));
        assert_eq!((life.claim, life.mapping, life.ticket), (0, 0, 0));
        assert_eq!(fake.calls,
                   ["reset-start", "reset-poll", "unmap", "unmap", "release", "release"]);
    }
}
