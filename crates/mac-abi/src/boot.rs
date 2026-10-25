//! Safe validation of the bootstrap's one physical memory account.
use crate::BootMemoryRange;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryError
{
    MissingRam,
    InvalidArray,
    InvalidRange,
    Unaligned,
    Overlap,
    OutsideRam,
    InconsistentSummary,
    PlatformMismatch,
    MissingReservation
}
impl core::fmt::Display for MemoryError
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        f.write_str(match self
                    {
                        Self::MissingRam => "bootstrap reports no RAM banks",
                        Self::PlatformMismatch =>
                        {
                            "bootstrap display-scratch handoff disagrees with the selected \
                             platform; rebuild ROM and disk together"
                        }
                        Self::MissingReservation =>
                        {
                            "bootstrap display-scratch handoff is not reserved; rebuild ROM and \
                             disk together"
                        }
                        Self::InvalidArray =>
                        {
                            "bootstrap memory array address, alignment or count is invalid"
                        }
                        Self::InvalidRange => "bootstrap memory extent is empty or overflows",
                        Self::Unaligned => "bootstrap memory extent is not page aligned",
                        Self::Overlap => "bootstrap RAM banks or reservations overlap",
                        Self::OutsideRam => "bootstrap reservation lies outside discovered RAM",
                        Self::InconsistentSummary =>
                        {
                            "bootstrap primary RAM summary disagrees with its bank list"
                        }
                    })
    }
}
pub struct Memory<'a>
{
    pub ram:          &'a [BootMemoryRange],
    pub reservations: &'a [BootMemoryRange]
}
fn end(range: BootMemoryRange, page_bytes: u64) -> Result<u64, MemoryError>
{
    let limit = range.base
                     .checked_add(range.length)
                     .filter(|_| range.length != 0)
                     .ok_or(MemoryError::InvalidRange)?;
    if !page_bytes.is_power_of_two()
       || !range.base.is_multiple_of(page_bytes)
       || !limit.is_multiple_of(page_bytes)
    {
        return Err(MemoryError::Unaligned);
    }
    Ok(limit)
}
impl Memory<'_>
{
    /// Validate the graphical scratch loan against the build-selected range.
    /// This validates the whole RAM/reservation account before the loan;
    /// callers do not have a separate validation step to remember.
    pub fn display_handoff(&self, page_bytes: u64, primary: BootMemoryRange,
                           declared: BootMemoryRange, expected: BootMemoryRange)
                           -> Result<(), MemoryError>
    {
        self.validate(page_bytes, primary)?;
        if declared != expected
        {
            return Err(MemoryError::PlatformMismatch);
        }
        let limit = end(expected, page_bytes)?;
        for reservation in self.reservations
        {
            if reservation.base <= expected.base && end(*reservation, page_bytes)? >= limit
            {
                return Ok(());
            }
        }
        Err(MemoryError::MissingReservation)
    }

    pub fn validate(&self, page_bytes: u64, primary: BootMemoryRange) -> Result<(), MemoryError>
    {
        let first = self.ram.first().ok_or(MemoryError::MissingRam)?;
        if *first != primary
        {
            return Err(MemoryError::InconsistentSummary);
        }
        for (index, bank) in self.ram.iter().copied().enumerate()
        {
            let limit = end(bank, page_bytes)?;
            for previous in &self.ram[..index]
            {
                if previous.base < limit && bank.base < end(*previous, page_bytes)?
                {
                    return Err(MemoryError::Overlap);
                }
            }
        }
        for (index, reserved) in self.reservations.iter().copied().enumerate()
        {
            let limit = end(reserved, page_bytes)?;
            if !self.ram.iter().any(|bank| {
                                   bank.base <= reserved.base
                                   && bank.base
                                          .checked_add(bank.length)
                                          .is_some_and(|end| limit <= end)
                               })
            {
                return Err(MemoryError::OutsideRam);
            }
            for previous in &self.reservations[..index]
            {
                if previous.base < limit && reserved.base < end(*previous, page_bytes)?
                {
                    return Err(MemoryError::Overlap);
                }
            }
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests
{
    extern crate alloc;
    use alloc::string::ToString;

    use super::*;
    const RAM: BootMemoryRange = BootMemoryRange { base:   0x80000000,
                                                   length: 0x200000000 };
    #[test]
    fn validates_large_ram_and_rejects_inconsistent_or_unusable_accounts()
    {
        let reserved = BootMemoryRange { base:   RAM.base,
                                         length: 4096 };
        let view = Memory { ram:          &[RAM],
                            reservations: &[reserved] };
        view.validate(4096, RAM).unwrap();
        assert_eq!(view.validate(4096, reserved), Err(MemoryError::InconsistentSummary));
        assert!(MemoryError::InconsistentSummary.to_string().contains("disagrees"));
        let cases = [(BootMemoryRange { base:   RAM.base + 1,
                                        length: 4096 },
                      MemoryError::Unaligned),
                     (BootMemoryRange { base:   RAM.base,
                                        length: 0 },
                      MemoryError::InvalidRange),
                     (BootMemoryRange { base:   u64::MAX - 4095,
                                        length: 4096 },
                      MemoryError::InvalidRange),
                     (BootMemoryRange { base:   0,
                                        length: 4096 },
                      MemoryError::OutsideRam)];
        for (range, error) in cases
        {
            assert_eq!(Memory { ram:          &[RAM],
                                reservations: &[range] }.validate(4096, RAM),
                       Err(error));
        }
        assert_eq!(Memory { ram:          &[RAM],
                            reservations: &[reserved, reserved] }.validate(4096, RAM),
                   Err(MemoryError::Overlap));
        assert_eq!(Memory { ram:          &[RAM, RAM],
                            reservations: &[] }.validate(4096, RAM),
                   Err(MemoryError::Overlap));
        assert_eq!(Memory { ram:          &[],
                            reservations: &[] }.validate(4096, RAM),
                   Err(MemoryError::MissingRam));
    }
}
