//! Borrowed container calculation contract. System supplies immutable child
//! requirements and scratch output, with no object/store borrows or callbacks.
//! The CDEF owns policy. This adapter validates wire input before author access.
use mac_abi::controls::reflow as wire;

use crate::{AxisConstraint, Error, Result, TrackSize};

#[derive(Clone, Copy)]
pub struct Track
{
    pub size:      TrackSize,
    pub minimum:   i64,
    pub preferred: i64,
    pub assigned:  i64,
    pub position:  i64
}
#[derive(Clone, Copy)]
pub struct Item
{
    pub start:     usize,
    pub span:      usize,
    pub minimum:   i64,
    pub preferred: i64,
    pub leading:   bool,
    pub trailing:  bool
}
/// No allocation, raw pointers, service calls or handle resolution. The lifetime
/// is one synchronous invocation and outputs remain private until admission.
pub struct AxisCalculation<'a>
{
    constraint: AxisConstraint,
    tracks:     &'a mut [wire::GridTrack],
    items:      &'a mut [wire::GridItem],
    minimum:    &'a mut i64,
    preferred:  &'a mut i64
}
impl<'a> AxisCalculation<'a>
{
    pub fn decode(constraint: wire::AxisConstraint, tracks: &'a mut [wire::GridTrack],
                  items: &'a mut [wire::GridItem], minimum: &'a mut i64, preferred: &'a mut i64)
                  -> Result<Self>
    {
        let constraint = constraint.try_into()?;
        let mut percent = 0u32;
        for t in &*tracks
        {
            TrackSize::decode(t.mode, t.percent)?;
            percent = percent.checked_add(t.percent).ok_or(Error::ExtentOverflow)?;
        }
        if percent > 10_000
        {
            return Err(Error::InvalidEncoding);
        }
        for item in &*items
        {
            if item.count == 0
               || item.start.checked_add(item.count).is_none_or(|end| end as usize > tracks.len())
               || item.minimum < 0
               || item.preferred < item.minimum
               || item.anchors & !3 != 0
               || item.reserved != 0
            {
                return Err(Error::InvalidEncoding);
            }
        }
        Ok(Self { constraint,
                  tracks,
                  items,
                  minimum,
                  preferred })
    }

    pub fn constraint(&self) -> AxisConstraint
    {
        self.constraint
    }

    pub fn track_count(&self) -> usize
    {
        self.tracks.len()
    }

    pub fn item_count(&self) -> usize
    {
        self.items.len()
    }

    pub fn track(&self, index: usize) -> Result<Track>
    {
        let t = self.tracks.get(index).ok_or(Error::InvalidEncoding)?;
        Ok(Track { size:      TrackSize::decode(t.mode, t.percent)?,
                   minimum:   t.minimum,
                   preferred: t.preferred,
                   assigned:  t.assigned,
                   position:  t.position })
    }

    pub fn item(&self, index: usize) -> Result<Item>
    {
        let i = self.items.get(index).ok_or(Error::InvalidEncoding)?;
        Ok(Item { start:     i.start as usize,
                  span:      i.count as usize,
                  minimum:   i.minimum,
                  preferred: i.preferred,
                  leading:   i.anchors & 1 != 0,
                  trailing:  i.anchors & 2 != 0 })
    }

    pub fn set_track(&mut self, index: usize, minimum: i64, preferred: i64, assigned: i64)
                     -> Result<()>
    {
        if minimum < 0 || preferred < minimum || assigned < 0
        {
            return Err(Error::InvalidRequirements);
        }
        let t = self.tracks.get_mut(index).ok_or(Error::InvalidEncoding)?;
        t.minimum = minimum;
        t.preferred = preferred;
        t.assigned = assigned;
        Ok(())
    }

    pub fn set_position(&mut self, index: usize, position: i64) -> Result<()>
    {
        if position < 0
        {
            return Err(Error::InvalidEncoding);
        }
        self.tracks.get_mut(index).ok_or(Error::InvalidEncoding)?.position = position;
        Ok(())
    }

    pub fn place(&mut self, index: usize, position: i64, extent: i64) -> Result<()>
    {
        if position < 0 || extent < 0 || position.checked_add(extent).is_none()
        {
            return Err(Error::ExtentOverflow);
        }
        let i = self.items.get_mut(index).ok_or(Error::InvalidEncoding)?;
        i.position = position;
        i.extent = extent;
        Ok(())
    }

    pub fn finish(&mut self, minimum: i64, preferred: i64) -> Result<()>
    {
        if minimum < 0 || preferred < minimum
        {
            return Err(Error::InvalidRequirements);
        }
        *self.minimum = minimum;
        *self.preferred = preferred;
        Ok(())
    }
}
