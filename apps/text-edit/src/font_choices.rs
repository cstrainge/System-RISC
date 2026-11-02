//! Bounded, revision-stable snapshots of the resident available-family catalog.
//! Up to 256 families share 32 KiB of UTF-8 names without an application heap.
//! Capacity exhaustion and catalog churn return errors, not partial lists.
use mac_toolbox::{Error, System};

pub const SIZES: [i16; 6] = [9, 10, 12, 14, 18, 24];
const MAX_FAMILIES: usize = 256;
const MAX_NAME_BYTES: usize = 32 * 1024;
const SNAPSHOT_ATTEMPTS: usize = 3;

#[derive(Clone, Copy, Default)]
struct Row
{
    family: i16,
    start:  u16,
    length: u16
}

pub struct Choice<'a>
{
    pub family: i16,
    pub name:   &'a str
}

pub struct Families
{
    pub revision: u32,
    rows:         [Row; MAX_FAMILIES],
    names:        [u8; MAX_NAME_BYTES],
    count:        u32
}

impl Families
{
    pub fn read(system: &System<'_>) -> Result<Self, Error>
    {
        let fonts = system.quickdraw().fonts();
        for _ in 0..SNAPSHOT_ATTEMPTS
        {
            let revision = fonts.revision()?;
            let count = fonts.count()?;
            if count as usize > MAX_FAMILIES
            {
                if fonts.revision()? != revision
                {
                    continue;
                }
                return Err(Error::NoMemory);
            }
            let mut snapshot = Self { revision,
                                      rows: [Row::default(); MAX_FAMILIES],
                                      names: [0; MAX_NAME_BYTES],
                                      count };
            let mut name_bytes = 0;
            let mut failed = None;
            for index in 0..count
            {
                match fonts.family(index)
                {
                    Ok(family) =>
                    {
                        if family.name_length > family.name.len()
                           || family.name().is_empty()
                           || snapshot.rows[..index as usize].iter().any(|row| {
                                                                        row.family
                                                                        == family.selection.family
                                                                    })
                        {
                            failed = Some(Error::CorruptData);
                            break;
                        }
                        let name = family.name().as_bytes();
                        let end = name_bytes + name.len();
                        if end > MAX_NAME_BYTES
                        {
                            failed = Some(Error::NoMemory);
                            break;
                        }
                        snapshot.names[name_bytes..end].copy_from_slice(name);
                        snapshot.rows[index as usize] = Row { family: family.selection.family,
                                                              start:  name_bytes as u16,
                                                              length: name.len() as u16 };
                        name_bytes = end;
                    }
                    Err(error) =>
                    {
                        failed = Some(error);
                        break;
                    }
                }
            }
            if fonts.revision()? != revision
            {
                continue;
            }
            if let Some(error) = failed
            {
                return Err(error);
            }
            return Ok(snapshot);
        }
        Err(Error::ScanStale)
    }

    pub fn len(&self) -> u32
    {
        self.count
    }

    pub fn get(&self, index: u32) -> Result<Choice<'_>, Error>
    {
        if index >= self.count
        {
            return Err(Error::ScanStale);
        }
        let row = self.rows[index as usize];
        let start = usize::from(row.start);
        let name = core::str::from_utf8(&self.names[start..start + usize::from(row.length)])
            .map_err(|_| Error::CorruptData)?;
        Ok(Choice { family: row.family,
                    name })
    }

    pub fn position(&self, family: i16) -> Option<u32>
    {
        self.rows[..self.count as usize].iter()
                                        .position(|row| row.family == family)
                                        .map(|row| row as u32)
    }

    pub fn same_rows(&self, other: &Self) -> bool
    {
        self.count == other.count
        && (0..self.count).all(|index| {
                              match (self.get(index), other.get(index))
                              {
                                  (Ok(a), Ok(b)) => a.family == b.family && a.name == b.name,
                                  _ => false
                              }
                          })
    }
}
