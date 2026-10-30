//! Checked native application service requirements. Selection and lifetime
//! admission belong to System; decoding this resource grants no authority.
use mac_module::ClassId;

use crate::resource::Resource;

pub const RESOURCE_KIND: [u8; 4] = *b"NREQ";
pub const RESOURCE_ID: i16 = 0;

pub const SCHEMA: u16 = 1;
pub const MAX_RECORDS: usize = 32;
pub const HEADER_BYTES: usize = 16;
pub const RECORD_BYTES: usize = 8;
pub const MAX_BYTES: usize = HEADER_BYTES + RECORD_BYTES * MAX_RECORDS;

/// One required interface class and exact nonzero version. Unknown classes are
/// valid format data; only genuine System admission can establish availability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Requirement
{
    pub class:   ClassId,
    pub version: u32
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    Resource,
    Header,
    Schema,
    Limit,
    Length,
    Reserved,
    Class,
    Version,
    Duplicate
}

impl core::fmt::Display for Error
{
    fn fmt(&self, out: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        out.write_str(match self
                      {
                          Self::Resource =>
                          {
                              "native service requirements need one uncompressed NREQ resource \
                               with ID 0 and zero attributes"
                          }
                          Self::Header => "native service requirements have an invalid header",
                          Self::Schema =>
                          {
                              "native service requirements use an unsupported schema; rebuild the \
                               package"
                          }
                          Self::Limit =>
                          {
                              "native service requirements exceed the bounded record limit"
                          }
                          Self::Length =>
                          {
                              "native service requirements length differs from the declared record \
                               count"
                          }
                          Self::Reserved =>
                          {
                              "native service requirements contain nonzero reserved fields"
                          }
                          Self::Class => "native service requirement has a zero interface class",
                          Self::Version =>
                          {
                              "native service requirement has a zero interface version"
                          }
                          Self::Duplicate =>
                          {
                              "native service requirements contain duplicate or conflicting classes"
                          }
                      })
    }
}
impl core::error::Error for Error
{
}

#[derive(Clone, Copy, Debug)]
pub struct Requirements<'a>
{
    records: &'a [u8]
}

impl<'a> Requirements<'a>
{
    /// Missing NREQ means no declared provider requirements. Any present NREQ
    /// must be the sole ID-zero resource; wrong IDs are never silently ignored.
    pub fn from_resources(resources: &'a [Resource]) -> Result<Self, Error>
    {
        let mut matching = resources.iter().filter(|row| row.kind == RESOURCE_KIND);
        let Some(row) = matching.next()
        else
        {
            return Ok(Self { records: &[] });
        };
        if row.id != RESOURCE_ID || row.attributes != 0 || matching.next().is_some()
        {
            return Err(Error::Resource);
        }
        Self::decode(&row.data)
    }

    /// Decode without allocation or provider lookup. This is metadata validation,
    /// not a readiness check or retained provider/dependency lease.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, Error>
    {
        if bytes.len() < HEADER_BYTES || bytes[..4] != *b"NREQ"
        {
            return Err(Error::Header);
        }
        if bytes.len() > MAX_BYTES
        {
            return Err(Error::Limit);
        }
        if u16::from_be_bytes([bytes[4], bytes[5]]) != SCHEMA
        {
            return Err(Error::Schema);
        }
        let count = usize::from(u16::from_be_bytes([bytes[6], bytes[7]]));
        if count > MAX_RECORDS
        {
            return Err(Error::Limit);
        }
        if bytes.len() != HEADER_BYTES + RECORD_BYTES * count
        {
            return Err(Error::Length);
        }
        if bytes[8..16] != [0; 8]
        {
            return Err(Error::Reserved);
        }
        let result = Self { records: &bytes[HEADER_BYTES..] };
        for (index, item) in result.iter().enumerate()
        {
            validate(item)?;
            if result.iter().take(index).any(|prior| prior.class == item.class)
            {
                return Err(Error::Duplicate);
            }
        }
        Ok(result)
    }

    pub fn iter(self) -> impl ExactSizeIterator<Item = Requirement> + 'a
    {
        self.records.chunks_exact(RECORD_BYTES).map(|row| {
                                                   Requirement { class:   ClassId([row[0],
                                                                                   row[1],
                                                                                   row[2], row[3]]),
                                                                 version:
                                                                     u32::from_be_bytes([row[4],
                                                                                         row[5],
                                                                                         row[6],
                                                                                         row[7]]) }
                                               })
    }
}
fn validate(item: Requirement) -> Result<(), Error>
{
    if item.class.0 == [0; 4]
    {
        return Err(Error::Class);
    }
    if item.version == 0
    {
        return Err(Error::Version);
    }
    Ok(())
}
/// Encode only after validating all records; refusal leaves output unchanged.
pub fn encode(records: &[Requirement], output: &mut [u8]) -> Result<usize, Error>
{
    if records.len() > MAX_RECORDS
    {
        return Err(Error::Limit);
    }
    let length = HEADER_BYTES + records.len() * RECORD_BYTES;
    if output.len() < length
    {
        return Err(Error::Length);
    }
    for (index, item) in records.iter().copied().enumerate()
    {
        validate(item)?;
        if records[..index].iter().any(|prior| prior.class == item.class)
        {
            return Err(Error::Duplicate);
        }
    }
    let output = &mut output[..length];
    output.fill(0);
    output[..4].copy_from_slice(b"NREQ");
    output[4..6].copy_from_slice(&SCHEMA.to_be_bytes());
    output[6..8].copy_from_slice(&(records.len() as u16).to_be_bytes());
    for (row, item) in output[HEADER_BYTES..].chunks_exact_mut(RECORD_BYTES).zip(records)
    {
        row[..4].copy_from_slice(&item.class.0);
        row[4..].copy_from_slice(&item.version.to_be_bytes());
    }
    Ok(length)
}
