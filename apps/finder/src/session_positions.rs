//! Read-only media can retain presentation state without writing to the media.
//! Keys contain the complete mount identity, so eject/remount cannot alias it.
#![forbid(unsafe_code)]
use mac_toolbox::{Cnid, Error, Point, VolumeId,
                  files::{IconMove, Placement}};
use spinning_top::Spinlock;

use crate::selection::CAPACITY;
#[derive(Clone, Copy)]
struct Position
{
    volume: u32,
    id:     u32,
    parent: u32,
    point:  Point
}
const EMPTY: Position = Position { volume: 0,
                                   id:     0,
                                   parent: 0,
                                   point:  Point { h: 0,
                                                   v: 0 } };
struct Store
{
    count:  usize,
    values: [Position; CAPACITY]
}
static POSITIONS: Spinlock<Store> = Spinlock::new(Store { count:  0,
                                                          values: [EMPTY; CAPACITY] });
impl Store
{
    fn find(&self, volume: u32, id: u32) -> Result<usize, usize>
    {
        self.values[..self.count].binary_search_by_key(&(volume, id), |p| (p.volume, p.id))
    }
}
pub fn get(volume: VolumeId, id: Cnid, parent: Cnid) -> Option<Point>
{
    let s = POSITIONS.lock();
    let p = &s.values[s.find(volume.0, id.0).ok()?];
    (p.parent == parent.0).then_some(p.point)
}
/// Release only records whose complete mount identities have retired.
pub fn retain(active: &[VolumeId])
{
    let mut s = POSITIONS.lock();
    let mut next = 0;
    for old in 0..s.count
    {
        let value = s.values[old];
        if active.binary_search_by_key(&value.volume, |id| id.0).is_ok()
        {
            s.values[next] = value;
            next += 1;
        }
    }
    s.count = next;
}
#[inline(never)]
pub fn save(volume: VolumeId, parent: Cnid, rows: &[IconMove]) -> Result<(), Error>
{
    if rows.len() > CAPACITY
    {
        return Err(Error::NoMemory);
    }
    let mut incoming = [EMPTY; CAPACITY];
    for (out, r) in incoming.iter_mut().zip(rows)
    {
        let Placement::At(point) = r.placement
        else
        {
            return Err(Error::BadArgument);
        };
        if r.expected_parent != parent
           || r.parent != parent
           || r.to_trash
           || point.h.checked_add(48).is_none()
           || point.v.checked_add(60).is_none()
        {
            return Err(Error::BadArgument);
        }
        *out = Position { volume: volume.0,
                          id: r.id.0,
                          parent: parent.0,
                          point };
    }
    let key = |p: &Position| (p.volume, p.id);
    incoming[..rows.len()].sort_unstable_by_key(key);
    if incoming[..rows.len()].windows(2).any(|pair| key(&pair[0]) == key(&pair[1]))
    {
        return Err(Error::BadArgument);
    }
    let mut s = POSITIONS.lock();
    let mut merged = [EMPTY; CAPACITY];
    let (mut old, mut new, mut count) = (0, 0, 0);
    while old < s.count || new < rows.len()
    {
        if count == CAPACITY
        {
            return Err(Error::NoMemory);
        }
        merged[count] = if new == rows.len()
                           || old < s.count && key(&s.values[old]) < key(&incoming[new])
        {
            let value = s.values[old];
            old += 1;
            value
        }
        else
        {
            if old < s.count && key(&s.values[old]) == key(&incoming[new])
            {
                old += 1;
            }
            let value = incoming[new];
            new += 1;
            value
        };
        count += 1;
    }
    s.values[..count].copy_from_slice(&merged[..count]);
    s.count = count;
    Ok(())
}
