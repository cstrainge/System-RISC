//! Shared resident object IDs: high16 generation, low16 one-based slot.
//! A removed/cleared ID never becomes live again in this table. Exhausted slots
//! are retired permanently rather than wrapping. Managers retain owner checks.
#![forbid(unsafe_code)]
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU16, Ordering};

// App-local tables share generation histories so the same slot in two apps
// cannot issue the same ID. The classic memory wire contract stays unchanged.
static EDITOR_GENERATIONS: [AtomicU16; mac_abi::textedit::MAX_EDITORS] =
    [const { AtomicU16::new(0) }; mac_abi::textedit::MAX_EDITORS];
static MENU_GENERATIONS: [AtomicU16; 8] = [const { AtomicU16::new(0) }; 8];

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
    Windows,
    Files,
    FileWrites,
    StandardFiles,
    ScrollBoxes,
    Controls,
    Volumes,
    TextEdit,
    Menus
}
impl Kind
{
    pub(crate) const fn capacity(self) -> usize
    {
        match self
        {
            Self::Menus => 8,
            Self::TextEdit => mac_abi::textedit::MAX_EDITORS,
            Self::Volumes => 128,
            Self::ScrollBoxes => 96,
            Self::Controls => 256,
            Self::Memory => 512,
            Self::Windows => 32,
            Self::Files => 64,
            Self::FileWrites | Self::StandardFiles => 8
        }
    }

    const fn first_slot(self) -> u32
    {
        match self
        {
            Self::Menus => Self::TextEdit.first_slot() + Self::TextEdit.capacity() as u32,
            Self::TextEdit => Self::Volumes.first_slot() + Self::Volumes.capacity() as u32,
            Self::Volumes => Self::Controls.first_slot() + Self::Controls.capacity() as u32,
            Self::Controls => Self::ScrollBoxes.first_slot() + Self::ScrollBoxes.capacity() as u32,
            Self::ScrollBoxes =>
            {
                Self::StandardFiles.first_slot() + Self::StandardFiles.capacity() as u32
            }
            Self::Memory => 1,
            Self::Windows => Self::Memory.capacity() as u32 + 1,
            Self::Files => Self::Windows.first_slot() + Self::Windows.capacity() as u32,
            Self::FileWrites => Self::Files.first_slot() + Self::Files.capacity() as u32,
            Self::StandardFiles =>
            {
                Self::FileWrites.first_slot() + Self::FileWrites.capacity() as u32
            }
        }
    }

    fn shared_generation(self, index: usize) -> Option<&'static AtomicU16>
    {
        match self
        {
            Self::TextEdit => EDITOR_GENERATIONS.get(index),
            Self::Menus => MENU_GENERATIONS.get(index),
            _ => None
        }
    }

    const fn last_generation(self) -> u16
    {
        match self
        {
            Self::Memory
            | Self::Files
            | Self::FileWrites
            | Self::StandardFiles
            | Self::ScrollBoxes
            | Self::Controls => u16::MAX,
            Self::Volumes | Self::TextEdit | Self::Menus => u16::MAX,
            Self::Windows => 255
        }
    }
}

/// Service-scoped identity storage; managers retain their existing owner policy.
/// Keep the table across resets and use clear(): replacing it resets identity
/// history. App-local kinds use centrally shared generation histories instead.
/// The central namespace registry also bounds generations for each ABI.
pub(crate) struct Table<T>
{
    kind:  Kind,
    slots: Vec<Slot<T>>
}
impl<T> Table<T>
{
    pub(crate) fn len(&self) -> usize
    {
        self.iter().count()
    }

    pub(crate) fn new(kind: Kind) -> Self
    {
        Self { kind,
               slots: Vec::new() }
    }

    fn available(&self, index: usize, slot: &Slot<T>) -> bool
    {
        slot.value.is_none()
        && self.kind
               .shared_generation(index)
               .map_or(slot.generation, |generation| generation.load(Ordering::Relaxed))
           < self.kind.last_generation()
    }

    fn vacant(&mut self) -> Result<usize, Exhausted>
    {
        if let Some(index) =
            self.slots.iter().enumerate().position(|(index, slot)| self.available(index, slot))
        {
            return Ok(index);
        }
        while self.slots.len() < self.kind.capacity()
        {
            self.slots.try_reserve(1).map_err(|_| Exhausted)?;
            self.slots.push(Slot { generation: 0,
                                   value:      None });
            let index = self.slots.len() - 1;
            if self.available(index, &self.slots[index])
            {
                return Ok(index);
            }
        }
        Err(Exhausted)
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
        loop
        {
            let index = self.vacant()?;
            let generation = if let Some(shared) = self.kind.shared_generation(index)
            {
                let Ok(previous) = shared.try_update(Ordering::Relaxed,
                                                     Ordering::Relaxed,
                                                     |generation| generation.checked_add(1))
                else
                {
                    continue;
                };
                previous + 1
            }
            else
            {
                self.slots[index].generation + 1
            };
            let slot = &mut self.slots[index];
            slot.generation = generation;
            let id = Self::id(self.kind, index, generation);
            slot.value = Some(create(id));
            return Ok(id);
        }
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
