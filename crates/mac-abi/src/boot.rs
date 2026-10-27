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
    MissingReservation,
    MissingImageArena
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
                        Self::MissingImageArena =>
                        {
                            "bootstrap flat image arena is not fully reserved; rebuild ROM and \
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
#[derive(Clone, Copy, Debug)]
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
impl<'a> Memory<'a>
{
    /// Validate the graphical scratch loan against the build-selected range.
    /// This validates the whole RAM/reservation account before the loan;
    /// callers do not have a separate validation step to remember.
    pub fn display_handoff(&self, page_bytes: u64, primary: BootMemoryRange,
                           declared: BootMemoryRange, expected: BootMemoryRange)
                           -> Result<CheckedMemory<'a>, MemoryError>
    {
        let memory = self.checked(page_bytes, primary)?;
        if declared != expected
        {
            return Err(MemoryError::PlatformMismatch);
        }
        let limit = end(expected, page_bytes)?;
        for reservation in self.reservations
        {
            if reservation.base <= expected.base && end(*reservation, page_bytes)? >= limit
            {
                return Ok(memory);
            }
        }
        Err(MemoryError::MissingReservation)
    }

    pub fn validate(&self, page_bytes: u64, primary: BootMemoryRange) -> Result<(), MemoryError>
    {
        self.checked(page_bytes, primary).map(|_| ())
    }

    /// Immutable proof over the original borrowed account; no second list of
    /// available pages is allocated or maintained by a consumer.
    pub fn checked(&self, page_bytes: u64, primary: BootMemoryRange)
                   -> Result<CheckedMemory<'a>, MemoryError>
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
        Ok(CheckedMemory { memory: *self,
                           page_bytes })
    }
}

/// Only validation constructs this view. Bank and reservation bytes stay
/// borrowed, so the proof cannot outlive them or coexist with their mutation.
#[derive(Clone, Copy, Debug)]
pub struct CheckedMemory<'a>
{
    memory:     Memory<'a>,
    page_bytes: u64
}
impl<'a> CheckedMemory<'a>
{
    pub fn banks(self) -> &'a [BootMemoryRange]
    {
        self.memory.ram
    }

    /// Whether one validated exclusion covers this whole platform reservation.
    pub fn reserved(self, range: BootMemoryRange) -> bool
    {
        let Some(end) = range.base.checked_add(range.length).filter(|_| range.length != 0)
        else
        {
            return false;
        };
        self.memory
            .reservations
            .iter()
            .any(|reserved| reserved.base <= range.base && end <= reserved.base + reserved.length)
    }

    pub const fn page_bytes(self) -> u64
    {
        self.page_bytes
    }

    /// Enumerates free spans, never individual pages. Unsorted reservations
    /// are accepted; each yielded extent is bounded by the next reservation.
    pub fn free_ranges(self, bank: usize) -> Option<FreeRanges<'a>>
    {
        let bank = *self.memory.ram.get(bank)?;
        Some(FreeRanges { reservations: self.memory.reservations,
                          cursor:       bank.base,
                          end:          bank.base + bank.length })
    }
}
#[derive(Clone)]
pub struct FreeRanges<'a>
{
    reservations: &'a [BootMemoryRange],
    cursor:       u64,
    end:          u64
}
impl Iterator for FreeRanges<'_>
{
    type Item = BootMemoryRange;

    fn next(&mut self) -> Option<Self::Item>
    {
        while self.cursor < self.end
        {
            let next =
                self.reservations
                    .iter()
                    .filter(|range| {
                        range.base < self.end && range.base + range.length > self.cursor
                    })
                    .min_by_key(|range| range.base);
            let start = self.cursor;
            if let Some(reserved) = next
            {
                self.cursor = reserved.base + reserved.length;
                if start < reserved.base
                {
                    return Some(BootMemoryRange { base:   start,
                                                  length: reserved.base - start });
                }
            }
            else
            {
                self.cursor = self.end;
                return Some(BootMemoryRange { base:   start,
                                              length: self.end - start });
            }
        }
        None
    }
}
impl core::iter::FusedIterator for FreeRanges<'_>
{
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
