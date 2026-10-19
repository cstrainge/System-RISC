//! Shared resident object IDs: high16 generation, low16 one-based slot.
//! A removed/cleared ID never becomes live again in this table. Exhausted slots
//! are retired permanently rather than wrapping. Managers retain owner checks.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

struct Slot<T>
{
    generation: u16,
    value:      Option<T>
}

#[derive(Debug)]
pub(crate) struct Exhausted;

/// Register object namespaces here, never in individual managers. Memory's
/// one-based slots1..512 are its unchanged wire contract; windows use a disjoint
/// range so a correctly formed live memory ID is still foreign to windows.
#[derive(Clone, Copy)]
pub(crate) enum Kind
{
    Memory,
    Windows
}
impl Kind
{
    pub(crate) const fn capacity(self) -> usize
    {
        match self
        {
            Self::Memory => 512,
            Self::Windows => 32
        }
    }

    const fn first_slot(self) -> u32
    {
        match self
        {
            Self::Memory => 1,
            Self::Windows => Self::Memory.capacity() as u32 + 1
        }
    }

    const fn last_generation(self) -> u16
    {
        match self
        {
            Self::Memory => u16::MAX,
            Self::Windows => 255
        }
    }
}

/// Service-scoped identity storage; managers retain their existing owner policy.
/// Keep the table across resets and use clear(): replacing it resets identity
/// history. The central namespace registry also bounds generations for each ABI.
pub(crate) struct Table<T>
{
    kind:  Kind,
    slots: Vec<Slot<T>>
}
impl<T> Table<T>
{
    pub(crate) fn new(kind: Kind) -> Self
    {
        Self { kind,
               slots: Vec::new() }
    }

    fn vacant(&mut self) -> Result<usize, Exhausted>
    {
        if let Some(index) =
            self.slots
                .iter()
                .position(|slot| {
                    slot.value.is_none() && slot.generation < self.kind.last_generation()
                })
        {
            return Ok(index);
        }
        if self.slots.len() >= self.kind.capacity()
        {
            return Err(Exhausted);
        }
        self.slots.try_reserve(1).map_err(|_| Exhausted)?;
        self.slots.push(Slot { generation: 0,
                               value:      None });
        Ok(self.slots.len() - 1)
    }

    /// Reserve capacity before a manager performs fallible object initialization.
    /// No ID/generation is consumed until insertion succeeds.
    pub(crate) fn reserve(&mut self) -> Result<(), Exhausted>
    {
        self.vacant().map(|_| ())
    }

    pub(crate) fn insert(&mut self, value: T) -> Result<u32, Exhausted>
    {
        self.insert_with(|_| value)
    }

    pub(crate) fn insert_with(&mut self, create: impl FnOnce(u32) -> T) -> Result<u32, Exhausted>
    {
        let index = self.vacant()?;
        let slot = &mut self.slots[index];
        slot.generation += 1;
        let id = Self::id(self.kind, index, slot.generation);
        slot.value = Some(create(id));
        Ok(id)
    }

    fn id(kind: Kind, index: usize, generation: u16) -> u32
    {
        (u32::from(generation) << 16) | (index as u32 + kind.first_slot())
    }

    fn index(&self, id: u32) -> Option<usize>
    {
        let index = (id & 0xffff).checked_sub(self.kind.first_slot())? as usize;
        let slot = self.slots.get(index)?;
        (u32::from(slot.generation) == id >> 16 && slot.value.is_some()).then_some(index)
    }

    pub(crate) fn get(&self, id: u32) -> Option<&T>
    {
        self.slots.get(self.index(id)?)?.value.as_ref()
    }

    pub(crate) fn get_mut(&mut self, id: u32) -> Option<&mut T>
    {
        let index = self.index(id)?;
        self.slots[index].value.as_mut()
    }

    pub(crate) fn remove(&mut self, id: u32) -> Option<T>
    {
        let index = self.index(id)?;
        self.slots[index].value.take()
    }

    pub(crate) fn clear(&mut self)
    {
        for slot in &mut self.slots
        {
            slot.value = None;
        }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (u32, &T)>
    {
        self.slots.iter().enumerate().filter_map(|(index, slot)| {
                                         slot.value.as_ref().map(|value| {
                                                                (Self::id(self.kind, index,
                                                                          slot.generation),
                                                                 value)
                                                            })
                                     })
    }

    pub(crate) fn values_mut(&mut self) -> impl Iterator<Item = &mut T>
    {
        self.slots.iter_mut().filter_map(|slot| slot.value.as_mut())
    }
}
