//! Enabled processor discovery metadata. This never selects a current CPU or
//! grants permission to execute, release a parked hart, or access its state.
#![forbid(unsafe_code)]
use core::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Cpu
{
    pub hart: u64
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    Empty,
    Duplicate
    {
        hart: u64
    },
    Unsorted
    {
        previous: u64,
        next:     u64
    },
    MissingPrimary
    {
        hart: u64
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Empty => out.write_str("boot CPU topology has no enabled processors"),
            Self::Duplicate { hart } => write!(out, "boot CPU topology repeats hart {hart}"),
            Self::Unsorted { previous,
                             next } =>
            {
                write!(out, "boot CPU topology is not sorted: hart {next} follows {previous}")
            }
            Self::MissingPrimary { hart } =>
            {
                write!(out, "boot CPU topology does not include primary hart {hart}")
            }
        }
    }
}

/// A borrowed, checked list, with no CPU-count cap or dense-ID assumption.
/// The actual execution boundary separately authenticates the current hart.
#[derive(Clone, Copy)]
pub struct Topology<'a>
{
    cpus: &'a [Cpu]
}
impl<'a> Topology<'a>
{
    pub fn checked(cpus: &'a [Cpu], primary: u64) -> Result<Self, Error>
    {
        if cpus.is_empty()
        {
            return Err(Error::Empty);
        }
        for pair in cpus.windows(2)
        {
            if pair[0].hart == pair[1].hart
            {
                return Err(Error::Duplicate { hart: pair[0].hart });
            }
            if pair[0].hart > pair[1].hart
            {
                return Err(Error::Unsorted { previous: pair[0].hart,
                                             next:     pair[1].hart });
            }
        }
        if cpus.binary_search_by_key(&primary, |cpu| cpu.hart).is_err()
        {
            return Err(Error::MissingPrimary { hart: primary });
        }
        Ok(Self { cpus })
    }

    pub fn cpus(self) -> &'a [Cpu]
    {
        self.cpus
    }
}
