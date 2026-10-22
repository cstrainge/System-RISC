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
    payload:   usize
}
impl Loader
{
    pub fn new(length: u64, budget: Budget) -> Result<Self>
    {
        if length < 16
        {
            return Err(Error("truncated resource header"));
        }
        Ok(Self { length,
                  budget,
                  stage: Stage::Header,
                  map: Vec::new(),
                  index: None,
                  resources: Vec::new(),
                  payload: 0 })
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

    fn next(&mut self, number: usize)
    {
        self.stage = if self.index.as_ref().is_some_and(|i| i.entry(number).is_some())
        {
            Stage::Length(number)
        }
        else
        {
            Stage::Done
        };
    }

    /// Supply one complete requested range. Storage failures are reported by the
    /// caller; short/oversized replies never advance the decoder.
    pub fn received(&mut self, bytes: &[u8]) -> Result<()>
    {
        if self.request().is_none_or(|(_, count)| count != bytes.len())
        {
            return Err(Error("resource storage read length mismatch"));
        }
        match self.stage
        {
            Stage::Header =>
            {
                let layout =
                    MapLayout::decode(bytes.try_into().unwrap(), self.length, self.budget)?;
                self.map
                    .try_reserve_exact(layout.map_length())
                    .map_err(|_| Error("resource allocation failed"))?;
                if layout.map_length() == 0
                {
                    return Err(Error("truncated resource map"));
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
                let index = self.index.as_mut().ok_or(Error("missing resource map"))?;
                index.record_length(number, length)?;
                let entry = index.entry(number).ok_or(Error("unknown resource entry"))?;
                let total = self.payload
                                .checked_add(length as usize)
                                .ok_or(Error("resource payload exceeds budget"))?;
                if total > self.budget.payload_bytes
                {
                    return Err(Error("resource payload exceeds budget"));
                }
                let name = entry.name
                                .map(|bytes| {
                                    let mut name = Vec::new();
                                    name.try_reserve_exact(bytes.len())
                                        .map_err(|_| Error("resource allocation failed"))?;
                                    name.extend_from_slice(bytes);
                                    Ok(name)
                                })
                                .transpose()?;
                let mut data = Vec::new();
                data.try_reserve_exact(length as usize)
                    .map_err(|_| Error("resource allocation failed"))?;
                self.resources.try_reserve(1).map_err(|_| Error("resource allocation failed"))?;
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
                let data =
                    &mut self.resources.last_mut().ok_or(Error("missing resource payload"))?.data;
                data.extend_from_slice(bytes);
                if data.len() == length
                {
                    self.next(number + 1);
                }
            }
            Stage::Done => return Err(Error("resource loader already complete"))
        }
        Ok(())
    }

    pub fn finish(self) -> Result<Vec<Resource>>
    {
        if !matches!(self.stage, Stage::Done)
        {
            return Err(Error("incomplete resource loading"));
        }
        Ok(self.resources)
    }
}

#[cfg(test)]
mod tests
{
    use alloc::vec;

    use super::*;
    fn fixture() -> Vec<Resource>
    {
        vec![Resource { kind:       *b"RV64",
                        id:         0,
                        name:       Some(vec![0x8e]),
                        attributes: 0x28,
                        data:       (0..CHUNK * 2 + 3).map(|i| i as u8).collect() },
             Resource { kind:       *b"STR ",
                        id:         -4,
                        name:       None,
                        attributes: 0,
                        data:       vec![] }]
    }
    fn load(bytes: &[u8], budget: Budget) -> Result<Vec<Resource>>
    {
        let mut loader = Loader::new(bytes.len() as u64, budget)?;
        while let Some((at, count)) = loader.request()
        {
            assert!(count > 0 && count <= CHUNK);
            loader.received(&bytes[at as usize..][..count])?;
        }
        loader.finish()
    }
    #[test]
    fn bounded_reads_preserve_every_resource_and_reject_incomplete_delivery()
    {
        let expected = fixture();
        let bytes = super::super::encode(&expected).unwrap();
        assert_eq!(load(&bytes, Budget::default()).unwrap(), expected);
        let mut loader = Loader::new(bytes.len() as u64, Budget::default()).unwrap();
        assert_eq!(loader.received(&bytes[..15]),
                   Err(Error("resource storage read length mismatch")));
        assert_eq!(loader.request(), Some((0, 16)));
        assert_eq!(loader.finish(), Err(Error("incomplete resource loading")));
    }
    #[test]
    fn large_maps_are_chunked_and_fork_padding_is_not_fetched()
    {
        let expected: Vec<_> = (0..256).map(|id| {
                                           Resource { kind: *b"TEST",
                                                      id,
                                                      attributes: id as u8,
                                                      name: Some(vec![b'x'; 255]),
                                                      data: vec![id as u8] }
                                       })
                                       .collect();
        let mut bytes = super::super::encode(&expected).unwrap();
        let logical_length = bytes.len();
        bytes.resize(logical_length + 1024 * 1024, 0xa5);
        let mut loader = Loader::new(bytes.len() as u64, Budget::default()).unwrap();
        let mut fetched = 0;
        while let Some((at, count)) = loader.request()
        {
            assert!(count <= CHUNK && at + count as u64 <= logical_length as u64);
            fetched += count;
            loader.received(&bytes[at as usize..][..count]).unwrap();
        }
        assert!(fetched < logical_length);
        assert_eq!(loader.finish().unwrap(), expected);
    }
    #[test]
    fn malformed_ranges_and_budgets_fail_before_payload_allocation()
    {
        let mut bytes = super::super::encode(&fixture()).unwrap();
        assert_eq!(load(&bytes,
                        Budget { payload_bytes: 1,
                                 ..Budget::default() }),
                   Err(Error("resource payload exceeds budget")));
        assert!(load(&bytes,
                     Budget { map_bytes: 1,
                              ..Budget::default() }).unwrap_err()
                                                    .0
                                                    .contains("budget"));
        let data = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
        bytes[data..data + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(load(&bytes, Budget::default()),
                   Err(Error("resource payload outside data region")));
        assert!(Loader::new(0, Budget::default()).is_err());
    }
}
