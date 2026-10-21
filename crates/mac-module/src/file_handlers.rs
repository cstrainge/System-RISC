//! Declarative FASS 0 resource. Associations are published only with the
//! owning module, and withdrawn as soon as that module leaves Ready state.
//! Icon IDs are local to the module's resource fork, never global aliases.
use crate::{Error, Result};
pub const HEADER_BYTES: usize = 8;
pub const RULE_BYTES: usize = 32;
pub const MAX_RULES: usize = 32;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum Action
{
    MountFilesystem = 1
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule
{
    pub file_type: [u8; 4],
    pub action:    Action,
    pub icon:      i16,
    extension:     [u8; 16],
    length:        u8
}
impl Rule
{
    pub fn new(file_type: [u8; 4], action: Action, icon: i16, extension: &str) -> Result<Self>
    {
        if !file_type.iter().all(u8::is_ascii_graphic)
           || extension.len() > 16
           || !extension.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            return Err(Error::Malformed);
        }
        let mut rule = Self { file_type,
                              action,
                              icon,
                              extension: [0; 16],
                              length: extension.len() as u8 };
        rule.extension[..extension.len()].copy_from_slice(extension.as_bytes());
        rule.extension.make_ascii_lowercase();
        Ok(rule)
    }

    pub fn extension(&self) -> &str
    {
        core::str::from_utf8(&self.extension[..self.length as usize]).unwrap_or("")
    }

    pub fn matches_extension(&self, name: &str) -> bool
    {
        self.length != 0
        && name.rsplit_once('.').is_some_and(|(_, ext)| ext.eq_ignore_ascii_case(self.extension()))
    }

    pub fn encode(self, out: &mut [u8]) -> Result<()>
    {
        let out: &mut [u8; RULE_BYTES] = out.try_into().map_err(|_| Error::Malformed)?;
        out.fill(0);
        out[..4].copy_from_slice(&self.file_type);
        out[4..6].copy_from_slice(&(self.action as u16).to_be_bytes());
        out[6..8].copy_from_slice(&self.icon.to_be_bytes());
        out[8] = self.length;
        out[16..].copy_from_slice(&self.extension);
        Ok(())
    }

    fn decode(bytes: &[u8]) -> Result<Self>
    {
        if bytes.len() != RULE_BYTES || bytes[9..16] != [0; 7] || bytes[8] > 16
        {
            return Err(Error::Malformed);
        }
        let length = bytes[8] as usize;
        if bytes[16 + length..].iter().any(|b| *b != 0)
        {
            return Err(Error::Malformed);
        }
        let action = match u16::from_be_bytes([bytes[4], bytes[5]])
        {
            1 => Action::MountFilesystem,
            _ => return Err(Error::Contract)
        };
        let rule = Self::new(bytes[..4].try_into().map_err(|_| Error::Malformed)?,
                             action,
                             i16::from_be_bytes([bytes[6], bytes[7]]),
                             core::str::from_utf8(&bytes[16..16 + length]).map_err(|_| {
                                                                              Error::Malformed
                                                                          })?)?;
        if rule.extension[..length] != bytes[16..16 + length]
        {
            return Err(Error::Malformed);
        }
        Ok(rule)
    }
}
pub struct Rules<'a>
{
    bytes: &'a [u8],
    count: usize
}
impl<'a> Rules<'a>
{
    pub fn decode(bytes: &'a [u8]) -> Result<Self>
    {
        if bytes.len() < HEADER_BYTES || &bytes[..4] != b"FAS1"
        {
            return Err(Error::Malformed);
        }
        if bytes[4..6] != 1u16.to_be_bytes()
        {
            return Err(Error::Version);
        }
        let count = u16::from_be_bytes([bytes[6], bytes[7]]) as usize;
        if count == 0 || count > MAX_RULES || bytes.len() != HEADER_BYTES + count * RULE_BYTES
        {
            return Err(Error::Limit);
        }
        let rules = Self { bytes,
                           count };
        for i in 0..count
        {
            let rule = rules.at(i)?;
            for j in 0..i
            {
                let previous = rules.at(j)?;
                if previous.file_type == rule.file_type
                   || !rule.extension().is_empty() && previous.extension() == rule.extension()
                {
                    return Err(Error::Duplicate);
                }
            }
        }
        Ok(rules)
    }

    pub fn count(&self) -> usize
    {
        self.count
    }

    pub fn at(&self, index: usize) -> Result<Rule>
    {
        if index >= self.count
        {
            return Err(Error::Limit);
        }
        let offset = HEADER_BYTES + index * RULE_BYTES;
        Rule::decode(&self.bytes[offset..offset + RULE_BYTES])
    }

    pub fn encode(rules: &[Rule], out: &mut [u8]) -> Result<()>
    {
        if rules.is_empty()
           || rules.len() > MAX_RULES
           || out.len() != HEADER_BYTES + rules.len() * RULE_BYTES
        {
            return Err(Error::Limit);
        }
        out[..4].copy_from_slice(b"FAS1");
        out[4..6].copy_from_slice(&1u16.to_be_bytes());
        out[6..8].copy_from_slice(&(rules.len() as u16).to_be_bytes());
        for (rule, bytes) in rules.iter().zip(out[HEADER_BYTES..].as_chunks_mut::<RULE_BYTES>().0)
        {
            rule.encode(bytes)?;
        }
        Rules::decode(out).map(|_| ())
    }
}
#[cfg(test)]
mod tests
{
    use super::*;
    #[test]
    fn type_action_and_local_icon_round_trip_with_strict_reserved_fields()
    {
        let rule = Rule::new(*b"SIT!", Action::MountFilesystem, 128, "SIT").unwrap();
        assert_eq!(rule.extension(), "sit");
        assert!(rule.matches_extension("Archive.SiT"));
        assert!(!rule.matches_extension("Archive.sitx"));
        let mut bytes = [0; 40];
        Rules::encode(&[rule], &mut bytes).unwrap();
        assert_eq!(Rules::decode(&bytes).unwrap().at(0), Ok(rule));
        for at in [0, 4, 8 + 9, 8 + 31]
        {
            let mut bad = bytes;
            bad[at] ^= 1;
            assert!(Rules::decode(&bad).is_err());
        }
        let mut bytes = [0; 72];
        assert!(Rules::encode(&[rule, rule], &mut bytes).is_err());
        assert!(Rule::new(*b"SIT!", Action::MountFilesystem, 128, ".sit").is_err());
    }
}
