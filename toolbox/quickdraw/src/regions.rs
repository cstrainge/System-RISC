//! Region calculations are performed by System into caller-provided storage.
use mac_abi::quickdraw::*;
use mac_traps::RegionCommand;

use crate::{Client, Error, Point, Rect, Result};
#[derive(Clone, Copy)]
pub struct Region<'a, 'b>
{
    client: Client<'a>,
    bytes:  &'b [u8]
}
impl Region<'_, '_>
{
    pub fn bytes(&self) -> &[u8]
    {
        self.bytes
    }

    pub fn contains(self, point: Point) -> Result<bool>
    {
        Ok(self.client
               .region(RegionCommand { operation: REGION_CONTAINS,
                                       a: self.bytes,
                                       point,
                                       ..Default::default() },
                       &mut [])?
               .1)
    }

    pub fn intersects(self, bounds: Rect) -> Result<bool>
    {
        Ok(self.client
               .region(RegionCommand { operation: REGION_INTERSECTS,
                                       a: self.bytes,
                                       bounds,
                                       ..Default::default() },
                       &mut [])?
               .1)
    }
}
#[derive(Clone, Copy)]
pub struct Regions<'a>
{
    client: Client<'a>
}
impl<'a> Regions<'a>
{
    pub const fn new(client: Client<'a>) -> Self
    {
        Self { client }
    }

    fn compute<'b>(self, command: RegionCommand<'_>, output: &'b mut [u8])
                   -> Result<Region<'a, 'b>>
    {
        let (length, _) = self.client.region(command, output)?;
        let bytes = output.get(..length).ok_or(Error::BadAbi)?;
        Ok(Region { client: self.client,
                    bytes })
    }

    pub fn parse<'b>(self, bytes: &'b [u8]) -> Result<Region<'a, 'b>>
    {
        self.client.region(RegionCommand { operation: REGION_CONTAINS,
                                            a: bytes,
                                            ..Default::default() },
                            &mut [])?;
        Ok(Region { client: self.client,
                    bytes })
    }

    pub fn rectangle(self, bounds: Rect, output: &mut [u8]) -> Result<Region<'a, '_>>
    {
        self.compute(RegionCommand { operation: REGION_RECT,
                                     bounds,
                                     ..Default::default() },
                     output)
    }

    pub fn oval(self, bounds: Rect, output: &mut [u8]) -> Result<Region<'a, '_>>
    {
        self.compute(RegionCommand { operation: REGION_OVAL,
                                     bounds,
                                     ..Default::default() },
                     output)
    }

    pub fn polygon<'b>(self, points: &[Point], output: &'b mut [u8]) -> Result<Region<'a, 'b>>
    {
        self.compute(RegionCommand { operation: REGION_POLYGON,
                                     points,
                                     ..Default::default() },
                     output)
    }

    pub fn intersection<'b>(self, a: Region<'_, '_>, b: Region<'_, '_>, output: &'b mut [u8])
                            -> Result<Region<'a, 'b>>
    {
        self.compute(RegionCommand { operation: REGION_INTERSECT,
                                     a: a.bytes,
                                     b: b.bytes,
                                     ..Default::default() },
                     output)
    }

    pub fn union<'b>(self, a: Region<'_, '_>, b: Region<'_, '_>, output: &'b mut [u8])
                     -> Result<Region<'a, 'b>>
    {
        self.compute(RegionCommand { operation: REGION_UNION,
                                     a: a.bytes,
                                     b: b.bytes,
                                     ..Default::default() },
                     output)
    }

    pub fn difference<'b>(self, a: Region<'_, '_>, b: Region<'_, '_>, output: &'b mut [u8])
                          -> Result<Region<'a, 'b>>
    {
        self.compute(RegionCommand { operation: REGION_DIFFERENCE,
                                     a: a.bytes,
                                     b: b.bytes,
                                     ..Default::default() },
                     output)
    }

    pub fn xor<'b>(self, a: Region<'_, '_>, b: Region<'_, '_>, output: &'b mut [u8])
                   -> Result<Region<'a, 'b>>
    {
        self.compute(RegionCommand { operation: REGION_XOR,
                                     a: a.bytes,
                                     b: b.bytes,
                                     ..Default::default() },
                     output)
    }

    pub fn offset<'b>(self, a: Region<'_, '_>, delta: Point, output: &'b mut [u8])
                      -> Result<Region<'a, 'b>>
    {
        self.compute(RegionCommand { operation: REGION_OFFSET,
                                     a: a.bytes,
                                     point: delta,
                                     ..Default::default() },
                     output)
    }

    pub fn inset<'b>(self, a: Region<'_, '_>, dh: i16, dv: i16, output: &'b mut [u8])
                     -> Result<Region<'a, 'b>>
    {
        self.compute(RegionCommand { operation: REGION_INSET,
                                     a: a.bytes,
                                     point: Point { h: dh,
                                                    v: dv },
                                     ..Default::default() },
                     output)
    }
}
impl<'a> crate::QuickDraw<'a>
{
    pub fn regions(self) -> Regions<'a>
    {
        Regions::new(self.client)
    }
}
