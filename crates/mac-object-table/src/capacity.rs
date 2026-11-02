//! Detached storage for the canonical treaty table. Capacity is not an ID or
//! a reservation of a slot; insertion always rechecks actual generation state.
use alloc::vec::Vec;
use core::{fmt, sync::atomic::Ordering};

use crate::{Kind, Slot, Table};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapacityError
{
    CapacityChanged,
    WrongKind,
    Exhausted,
    AllocationFailed
}
impl fmt::Display for CapacityError
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        out.write_str(match self
                      {
                          Self::CapacityChanged =>
                          {
                              "treaty table capacity changed; prepare again outside guards"
                          }
                          Self::WrongKind =>
                          {
                              "prepared treaty capacity belongs to a different namespace"
                          }
                          Self::Exhausted => "treaty namespace has no reusable slot or generation",
                          Self::AllocationFailed =>
                          {
                              "allocation of detached treaty table capacity failed"
                          }
                      })
    }
}
impl core::error::Error for CapacityError
{
}

pub struct CapacityPlan<T>
{
    kind:     Kind,
    required: usize,
    marker:   core::marker::PhantomData<T>
}
/// Retains any obsolete allocation after installation. Drop outside guards.
pub struct PreparedCapacity<T>
{
    kind:  Kind,
    slots: Vec<Slot<T>>
}
impl<T> CapacityPlan<T>
{
    pub fn prepare(self) -> Result<PreparedCapacity<T>, CapacityError>
    {
        let mut slots = Vec::new();
        slots.try_reserve_exact(self.required).map_err(|_| CapacityError::AllocationFailed)?;
        Ok(PreparedCapacity { kind: self.kind,
                              slots })
    }
}
impl<T> Table<T>
{
    pub(super) fn required_capacity(&self) -> Result<usize, CapacityError>
    {
        if let Some(index) =
            self.slots.iter().enumerate().position(|(index, slot)| self.available(index, slot))
        {
            return Ok(index + 1);
        }
        (self.slots.len()..self.kind.capacity()).find(|&index| {
                                                    self.kind
                                                        .shared_generation(index)
                                                        .load(Ordering::Relaxed)
                                                    < u16::MAX
                                                })
                                                .map(|index| index + 1)
                                                .ok_or(CapacityError::Exhausted)
    }

    pub fn plan_capacity(&self) -> Result<CapacityPlan<T>, CapacityError>
    {
        let required = self.required_capacity()?;
        Ok(CapacityPlan { kind:     self.kind,
                          required: if required <= self.slots.capacity() { 0 } else { required },
                          marker:   core::marker::PhantomData })
    }

    /// No allocation or payload destruction; the caller retains both buffers
    /// on refusal and receives the old empty allocation in `prepared` on success.
    pub fn admit_capacity(&mut self, prepared: &mut PreparedCapacity<T>)
                          -> Result<(), CapacityError>
    {
        if self.kind != prepared.kind
        {
            return Err(CapacityError::WrongKind);
        }
        let required = self.required_capacity()?;
        if required <= self.slots.capacity()
        {
            return Ok(());
        }
        if required > prepared.slots.capacity()
        {
            return Err(CapacityError::CapacityChanged);
        }
        prepared.slots.append(&mut self.slots);
        core::mem::swap(&mut self.slots, &mut prepared.slots);
        Ok(())
    }

    /// Uses already-installed capacity only. Refusal returns the original value.
    pub fn insert_prepared(&mut self, value: T) -> Result<u32, (CapacityError, T)>
    {
        match self.issue(false)
        {
            Ok((index, id)) =>
            {
                self.slots[index].value = Some(value);
                Ok(id)
            }
            Err(cause) => Err((cause, value))
        }
    }
}
