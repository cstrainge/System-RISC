//! Resumable, bounded application resource loading for synchronous and asynchronous
//! storage. Only the map and final payloads are retained, never a second fork copy.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use super::{Resource,
            index::{Index, MapLayout},
            stream::Budget};
use crate::{Error, Result};

/// Maximum bytes requested from storage in a single step.
pub const CHUNK: usize = 64 * 1024;
enum Stage
{
    Header,
    Map(MapLayout),
    Length(usize),
    Data
    {
        number: usize,
        offset: u64,
        length: usize
    },
    Done
}
/// Owned decoding state. Dropping it releases every partially loaded resource.
pub struct Loader
{
    length:    u64,
    budget:    Budget,
    stage:     Stage,
    map:       Vec<u8>,
    index:     Option<Index>,
    resources: Vec<Resource>,
    payload:   usize,
    select:    fn([u8; 4], i16) -> bool
}
impl Loader
{
    pub fn new(length: u64, budget: Budget) -> Result<Self>
    {
        Self::selected(length, budget, |_, _| true)
    }

    /// Metadata consumers select payloads before any payload length/read or
    /// allocation. The complete map is still validated with the same budgets.
    pub fn selected(length: u64, budget: Budget, select: fn([u8; 4], i16) -> bool) -> Result<Self>
    {
        if length < 16
        {
            return Err(Error::Malformed("truncated resource header"));
        }
        Ok(Self { length,
                  budget,
                  stage: Stage::Header,
                  map: Vec::new(),
                  index: None,
                  resources: Vec::new(),
                  payload: 0,
                  select })
    }

    /// The next exact positioned read. `None` means loading is complete.
    pub fn request(&self) -> Option<(u64, usize)>
    {
        match self.stage
        {
            Stage::Header => Some((0, 16)),
            Stage::Map(layout) =>
            {
                Some((layout.map_offset() + self.map.len() as u64,
                      (layout.map_length() - self.map.len()).min(CHUNK)))
            }
            Stage::Length(number) => self.index.as_ref()?.entry(number).map(|e| (e.offset, 4)),
            Stage::Data { offset,
                          length,
                          .. } =>
            {
                let received = self.resources.last()?.data.len();
                Some((offset + received as u64, (length - received).min(CHUNK)))
            }
            Stage::Done => None
        }
    }

    fn next(&mut self, mut number: usize)
    {
        while let Some(entry) = self.index.as_ref().and_then(|index| index.entry(number))
        {
            if (self.select)(entry.kind, entry.id)
            {
                self.stage = Stage::Length(number);
                return;
            }
            number += 1;
        }
        self.stage = Stage::Done;
    }

    /// Supply one complete requested range. Storage failures are reported by the
    /// caller; short/oversized replies never advance the decoder.
    pub fn received(&mut self, bytes: &[u8]) -> Result<()>
    {
        if self.request().is_none_or(|(_, count)| count != bytes.len())
        {
            return Err(Error::Malformed("resource storage read length mismatch"));
        }
        match self.stage
        {
            Stage::Header =>
            {
                let layout =
                    MapLayout::decode(bytes.try_into().unwrap(), self.length, self.budget)?;
                self.map
                    .try_reserve_exact(layout.map_length())
                    .map_err(|_| Error::AllocationFailed("resource allocation failed"))?;
                if layout.map_length() == 0
                {
                    return Err(Error::Malformed("truncated resource map"));
                }
                self.stage = Stage::Map(layout);
            }
            Stage::Map(layout) =>
            {
                self.map.extend_from_slice(bytes);
                if self.map.len() == layout.map_length()
                {
                    self.index = Some(Index::decode(layout, core::mem::take(&mut self.map))?);
                    self.next(0);
                }
            }
            Stage::Length(number) =>
            {
                let length = u32::from_be_bytes(bytes.try_into().unwrap());
                let index = self.index.as_mut().ok_or(Error::Malformed("missing resource map"))?;
                index.record_length(number, length)?;
                let entry =
                    index.entry(number).ok_or(Error::Malformed("unknown resource entry"))?;
                let total = self.payload
                                .checked_add(length as usize)
                                .ok_or(Error::BudgetExceeded("resource payload exceeds budget"))?;
                if total > self.budget.payload_bytes
                {
                    return Err(Error::BudgetExceeded("resource payload exceeds budget"));
                }
                let name = entry.name
                                .map(|bytes| {
                                    let mut name = Vec::new();
                                    name.try_reserve_exact(bytes.len())
                                        .map_err(|_| Error::AllocationFailed("resource allocation failed"))?;
                                    name.extend_from_slice(bytes);
                                    Ok::<_,Error>(name)
                                })
                                .transpose()?;
                let mut data = Vec::new();
                data.try_reserve_exact(length as usize)
                    .map_err(|_| Error::AllocationFailed("resource allocation failed"))?;
                self.resources
                    .try_reserve(1)
                    .map_err(|_| Error::AllocationFailed("resource allocation failed"))?;
                self.resources.push(Resource { kind: entry.kind,
                                               id: entry.id,
                                               attributes: entry.attributes,
                                               name,
                                               data });
                self.payload = total;
                let offset = entry.offset + 4;
                if length == 0
                {
                    self.next(number + 1);
                }
                else
                {
                    self.stage = Stage::Data { number,
                                               offset,
                                               length: length as usize };
                }
            }
            Stage::Data { number,
                          length,
                          .. } =>
            {
                let data = &mut self.resources
                                    .last_mut()
                                    .ok_or(Error::Malformed("missing resource payload"))?
                                    .data;
                data.extend_from_slice(bytes);
                if data.len() == length
                {
                    self.next(number + 1);
                }
            }
            Stage::Done => return Err(Error::Malformed("resource loader already complete"))
        }
        Ok(())
    }

    pub fn finish(self) -> Result<Vec<Resource>>
    {
        if !matches!(self.stage, Stage::Done)
        {
            return Err(Error::Malformed("incomplete resource loading"));
        }
        Ok(self.resources)
    }
}

