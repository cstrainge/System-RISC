//! Fixed-capacity byte ring. Architecture-neutral and allocation-free after
//! construction; policy, ownership and wakeups belong to the manager.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

pub(crate) struct Ring
{
    bytes: Vec<u8>,
    read:  usize,
    len:   usize
}
impl Ring
{
    /// Reserve exactly `capacity` bytes. A refused reservation leaves no ring.
    pub(crate) fn with_capacity(capacity: usize) -> Option<Self>
    {
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(capacity).ok()?;
        bytes.resize(capacity, 0);
        Some(Self { bytes,
                    read: 0,
                    len: 0 })
    }

    pub(crate) fn available(&self) -> usize
    {
        self.len
    }

    pub(crate) fn space(&self) -> usize
    {
        self.bytes.len() - self.len
    }

    /// Remove up to `output.len()` bytes in write order.
    pub(crate) fn read_into(&mut self, output: &mut [u8]) -> usize
    {
        let count = output.len().min(self.len);
        if count == 0
        {
            return 0;
        }
        let capacity = self.bytes.len();
        let first = (capacity - self.read).min(count);
        output[..first].copy_from_slice(&self.bytes[self.read..self.read + first]);
        if count > first
        {
            output[first..count].copy_from_slice(&self.bytes[..count - first]);
        }
        self.read = (self.read + count) % capacity;
        self.len -= count;
        count
    }

    /// Add as much of `input` as the free space allows, in write order.
    pub(crate) fn write_from(&mut self, input: &[u8]) -> usize
    {
        let count = input.len().min(self.space());
        if count == 0
        {
            return 0;
        }
        let capacity = self.bytes.len();
        let tail = (self.read + self.len) % capacity;
        let first = (capacity - tail).min(count);
        self.bytes[tail..tail + first].copy_from_slice(&input[..first]);
        if count > first
        {
            self.bytes[..count - first].copy_from_slice(&input[first..count]);
        }
        self.len += count;
        count
    }

    /// Discard buffered bytes; used when the last reader closes.
    pub(crate) fn clear(&mut self)
    {
        self.read = 0;
        self.len = 0;
    }
}

