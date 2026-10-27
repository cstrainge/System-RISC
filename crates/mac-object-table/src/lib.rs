//! Shared resident object IDs: high16 generation, low16 one-based slot.
//! A removed/cleared ID never becomes live again in this table. Exhausted slots
//! are retired permanently rather than wrapping. Managers retain owner checks.
#![no_std]
#![forbid(unsafe_code)]
#![feature(allocator_ext)]
extern crate alloc;
pub mod projected;
pub mod aliases;
pub mod completion;
pub mod retained;
pub mod scope;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU16, Ordering};

// All tables share boot-wide generation histories: reconstructed managers and
// concurrent instances cannot issue the same wire ID. Memory's contract stays unchanged.
static FILE_GENERATIONS: [AtomicU16; 64] = [const { AtomicU16::new(0) }; 64];
static VOLUME_GENERATIONS: [AtomicU16; 128] = [const { AtomicU16::new(0) }; 128];
static MEMORY_GENERATIONS: [AtomicU16; 512] = [const { AtomicU16::new(0) }; 512];
static DRIVER_REQUEST_GENERATIONS: [AtomicU16; 256] = [const { AtomicU16::new(0) }; 256];

static HOST_FILE_GENERATIONS: [AtomicU16; 1024] = [const { AtomicU16::new(0) }; 1024];
static HOST_DIRECTORY_GENERATIONS: [AtomicU16; 256] = [const { AtomicU16::new(0) }; 256];
static HOST_TRANSACTION_GENERATIONS: [AtomicU16; 64] = [const { AtomicU16::new(0) }; 64];
static HOST_SESSION_GENERATIONS: [AtomicU16; 16] = [const { AtomicU16::new(0) }; 16];

static DEVICE_GENERATIONS: [AtomicU16; 64] = [const { AtomicU16::new(0) }; 64];
static JOB_GENERATIONS: [AtomicU16; 128] = [const { AtomicU16::new(0) }; 128];
static DMA_GENERATIONS: [AtomicU16; 128] = [const { AtomicU16::new(0) }; 128];
static FILE_SOURCE_GENERATIONS: [AtomicU16; 16] = [const { AtomicU16::new(0) }; 16];
static HOST_NODE_GENERATIONS: [AtomicU16; 1024] = [const { AtomicU16::new(0) }; 1024];
static FILE_WATCH_GENERATIONS: [AtomicU16; 64] = [const { AtomicU16::new(0) }; 64];
static DRAG_GENERATIONS: [AtomicU16; 32] = [const { AtomicU16::new(0) }; 32];
static DRAG_RECEIVER_GENERATIONS: [AtomicU16; 64] = [const { AtomicU16::new(0) }; 64];

struct Slot<T>
{
    generation: u16,
    value:      Option<T>
}

#[derive(Debug)]
pub struct Exhausted;

/// Register object namespaces here, never in individual managers. Memory's
/// one-based slots1..512 are its unchanged wire contract. Migrated managers
/// use registered owner stores and full-width identities instead of these ranges.
#[derive(Clone, Copy)]
pub enum Kind
{
    Memory,
    Files,
    Volumes,
    DriverRequests,
    HostFiles,
    HostDirectories,
    HostTransactions,
    HostSessions,
    DriverDevices,
    DriverDma,
    DriverJobs,
    FileSources,
    HostNodes,
    FileWatches,
    Drags,
    DragReceivers
}
impl Kind
{
    pub const fn capacity(self) -> usize
    {
        match self
        {
            Self::Drags => 32,
            Self::DragReceivers => 64,
            Self::FileWatches => 64,
            Self::HostNodes => 1024,
            Self::FileSources => 16,
            Self::DriverJobs => 128,
            Self::DriverDevices => 64,
            Self::DriverDma => 128,
            Self::HostFiles => 1024,
            Self::HostDirectories => 256,
            Self::HostTransactions => 64,
            Self::HostSessions => 16,
            Self::DriverRequests => 256,
            Self::Volumes => 128,
            Self::Memory => 512,
            Self::Files => 64
        }
    }

    const fn first_slot(self) -> u32
    {
        match self
        {
            Self::Drags => Self::FileWatches.first_slot() + Self::FileWatches.capacity() as u32,
            Self::DragReceivers => Self::Drags.first_slot() + Self::Drags.capacity() as u32,
            Self::FileWatches => Self::HostNodes.first_slot() + Self::HostNodes.capacity() as u32,
            Self::HostNodes => Self::FileSources.first_slot() + Self::FileSources.capacity() as u32,
            Self::FileSources => Self::DriverJobs.first_slot() + Self::DriverJobs.capacity() as u32,
            Self::DriverJobs => Self::DriverDma.first_slot() + Self::DriverDma.capacity() as u32,
            Self::DriverDevices =>
            {
                Self::HostSessions.first_slot() + Self::HostSessions.capacity() as u32
            }
            Self::DriverDma =>
            {
                Self::DriverDevices.first_slot() + Self::DriverDevices.capacity() as u32
            }
            Self::HostFiles =>
            {
                Self::DriverRequests.first_slot() + Self::DriverRequests.capacity() as u32
            }
            Self::HostDirectories =>
            {
                Self::HostFiles.first_slot() + Self::HostFiles.capacity() as u32
            }
            Self::HostTransactions =>
            {
                Self::HostDirectories.first_slot() + Self::HostDirectories.capacity() as u32
            }
            Self::HostSessions =>
            {
                Self::HostTransactions.first_slot() + Self::HostTransactions.capacity() as u32
            }
            Self::DriverRequests => Self::Volumes.first_slot() + Self::Volumes.capacity() as u32,
            Self::Volumes => Self::Files.first_slot() + Self::Files.capacity() as u32,
            Self::Memory => 1,
            Self::Files => Self::Memory.capacity() as u32 + 1
        }
    }

    /// Classify an opaque wire ID's namespace, without asserting it is live.
    /// The owning table must still validate its full generation and owner.
    pub const fn contains_wire_id(self, id: u32) -> bool
    {
        let slot = id & 0xffff;
        id >> 16 != 0
        && slot >= self.first_slot()
        && slot < self.first_slot() + self.capacity() as u32
    }

    fn shared_generation(self, index: usize) -> &'static AtomicU16
    {
        match self
        {
            Self::Memory => &MEMORY_GENERATIONS[index],
            Self::Files => &FILE_GENERATIONS[index],
            Self::Volumes => &VOLUME_GENERATIONS[index],
            Self::Drags => &DRAG_GENERATIONS[index],
            Self::DragReceivers => &DRAG_RECEIVER_GENERATIONS[index],
            Self::FileWatches => &FILE_WATCH_GENERATIONS[index],
            Self::HostNodes => &HOST_NODE_GENERATIONS[index],
            Self::FileSources => &FILE_SOURCE_GENERATIONS[index],
            Self::DriverJobs => &JOB_GENERATIONS[index],
            Self::DriverDevices => &DEVICE_GENERATIONS[index],
            Self::DriverDma => &DMA_GENERATIONS[index],
            Self::HostFiles => &HOST_FILE_GENERATIONS[index],
            Self::HostDirectories => &HOST_DIRECTORY_GENERATIONS[index],
            Self::HostTransactions => &HOST_TRANSACTION_GENERATIONS[index],
            Self::HostSessions => &HOST_SESSION_GENERATIONS[index],
            Self::DriverRequests => &DRIVER_REQUEST_GENERATIONS[index]
        }
    }
}

/// Service-scoped identity storage; managers retain their existing owner policy.
/// Every namespace retains boot-wide generation history, including when a
/// manager is cleared, reconstructed, or exists alongside another instance.
/// The central namespace registry also bounds generations for each ABI.
pub struct Table<T>
{
    kind:  Kind,
    slots: Vec<Slot<T>>
}
impl<T> Table<T>
{
    pub fn len(&self) -> usize
    {
        self.iter().count()
    }

    pub fn is_empty(&self) -> bool
    {
        self.iter().next().is_none()
    }

    pub const fn new(kind: Kind) -> Self
    {
        Self { kind,
               slots: Vec::new() }
    }

    fn available(&self, index: usize, slot: &Slot<T>) -> bool
    {
        slot.value.is_none()
        && self.kind.shared_generation(index).load(Ordering::Relaxed) < u16::MAX
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
    pub fn reserve(&mut self) -> Result<(), Exhausted>
    {
        self.vacant().map(|_| ())
    }

    pub fn insert(&mut self, value: T) -> Result<u32, Exhausted>
    {
        self.insert_with(|_| value)
    }

    pub fn insert_with(&mut self, create: impl FnOnce(u32) -> T) -> Result<u32, Exhausted>
    {
        loop
        {
            let index = self.vacant()?;
            let shared = self.kind.shared_generation(index);
            let Ok(previous) = shared.try_update(Ordering::Relaxed,
                                                 Ordering::Relaxed,
                                                 |generation| generation.checked_add(1))
            else
            {
                continue;
            };
            let generation = previous + 1;
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

    pub fn get(&self, id: u32) -> Option<&T>
    {
        self.slots.get(self.index(id)?)?.value.as_ref()
    }

    pub fn get_mut(&mut self, id: u32) -> Option<&mut T>
    {
        let index = self.index(id)?;
        self.slots[index].value.as_mut()
    }

    pub fn remove(&mut self, id: u32) -> Option<T>
    {
        let index = self.index(id)?;
        self.slots[index].value.take()
    }

    pub fn clear(&mut self)
    {
        for slot in &mut self.slots
        {
            slot.value = None;
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (u32, &T)>
    {
        self.slots.iter().enumerate().filter_map(|(index, slot)| {
                                         slot.value.as_ref().map(|value| {
                                                                (Self::id(self.kind, index,
                                                                          slot.generation),
                                                                 value)
                                                            })
                                     })
    }

    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut T>
    {
        self.slots.iter_mut().filter_map(|slot| slot.value.as_mut())
    }
}

/// Next-model domain metadata; runtime adoption follows the staged rewrite.
pub mod domain;
pub mod shared;

pub mod store;
pub mod registry;
pub mod walk;
