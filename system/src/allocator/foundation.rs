//! Unique System Foundation owners for raw physical CPU backing. These bytes
//! come from the page pool, never the ordinary or protected allocator heap.
//! The physical lease is destroyed before its separate attribution receipt.
#![deny(unsafe_code)]
use core::{fmt, ops::Deref};

use super::budget;
use crate::machine::{pages::{self, Backing, Stack},
                     traps::{SavedState, TrapFrame}};

#[derive(Debug)]
pub(crate) enum Error
{
    Pages(pages::Error),
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
            Self::Pages(cause) => cause.status(),
            Self::CounterOverflow { .. } => mac_abi::NOT_SUPPORTED
        }
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Pages(cause) => cause.fmt(out),
            Self::CounterOverflow { used,
                                    requested } =>
            {
                write!(out,
                       "System Foundation physical attribution cannot represent {requested} \
                        additional bytes after {used}; no backing was published")
            }
        }
    }
}

/// No value extraction, mutable dereference or cloning can separate backing
/// from its original physical receipt. Only the fresh ordinary-storage
/// constructors below may issue one; protected and domain-charged values cannot
/// be passed in by callers for a second attribution.
pub(crate) struct Foundation<T>
{
    value:    T,
    _receipt: budget::FoundationPhysicalReceipt
}
impl<T> Foundation<T>
{
    fn charge(value: T, bytes: u64) -> Result<Self, Error>
    {
        let receipt = budget::reserve_foundation_physical(bytes)?;
        Ok(Self { value,
                  _receipt: receipt })
    }
}
impl<T> Deref for Foundation<T>
{
    type Target = T;

    fn deref(&self) -> &T
    {
        &self.value
    }
}
impl Foundation<Backing>
{
    /// Fresh raw metadata storage; the owner initializes every typed element
    /// before publishing a view. Attribution covers the full physical lease.
    pub(crate) fn reserve_uninitialized(bytes: usize) -> Result<Self, Error>
    {
        let backing = Backing::reserve_uninitialized(bytes).map_err(Error::Pages)?;
        let bytes = backing.bytes();
        Self::charge(backing, bytes)
    }

    pub(crate) fn allocate(bytes: usize) -> Result<Self, Error>
    {
        let backing = Backing::allocate(bytes).map_err(Error::Pages)?;
        let bytes = backing.bytes();
        Self::charge(backing, bytes)
    }
}
impl Foundation<Stack>
{
    pub(crate) fn allocate(bytes: usize) -> Result<Self, Error>
    {
        let stack = Stack::allocate(bytes).map_err(Error::Pages)?;
        let bytes = stack.backing_bytes();
        Self::charge(stack, bytes)
    }

    /// # Safety
    /// Both owned permanent roots already map this stack's physical extent at
    /// cpu_base. No executor, storage pointer/reference or CPU frame has been
    /// published for this stack. The caller keeps both roots and this owner
    /// pinned until reset. This changes only the future numeric executor
    /// address; the existing physical receipt and backing remain unchanged.
    #[allow(unsafe_code)]
    pub(crate) unsafe fn bind_mapping(&mut self, cpu_base: usize) -> Result<(), Error>
    {
        // SAFETY: the architecture owner supplies the prepublication mapping
        // contract above; no mutable Stack reference escapes this boundary.
        unsafe { self.value.bind_mapping(cpu_base) }.map_err(Error::Pages)
    }
}
impl Foundation<SavedState>
{
    pub(crate) fn new(initial: TrapFrame) -> Result<Self, Error>
    {
        let state = SavedState::new(initial).map_err(Error::Pages)?;
        let bytes = state.backing_bytes();
        Self::charge(state, bytes)
    }
}
