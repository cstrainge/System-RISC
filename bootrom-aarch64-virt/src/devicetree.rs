//! Read-only walk of the generated device tree.
//!
//! The bootloader consumes the tree rather than knowing peripheral addresses as
//! constants, so the platform record stays the only place a board's hardware is
//! written down. This is deliberately a small reader: it answers "where is the
//! node with this compatible string?" and nothing else. Unknown properties and
//! nodes are skipped rather than rejected, so a future platform's extras are
//! data rather than a bootloader change.

const MAGIC: u32 = 0xd00d_feed;
const BEGIN_NODE: u32 = 1;
const END_NODE: u32 = 2;
const PROP: u32 = 3;
const NOP: u32 = 4;
const END: u32 = 9;

pub struct Tree<'a>
{
    structure: &'a [u8],
    strings:   &'a [u8]
}

impl<'a> Tree<'a>
{
    /// Accepts a blob only when the header is self-consistent. A malformed tree
    /// would otherwise look like a machine with no devices.
    pub fn new(bytes: &'a [u8]) -> Option<Self>
    {
        if bytes.len() < 40
        {
            return None;
        }
        let word = |offset: usize| -> Option<u32> {
            Some(u32::from_be_bytes(bytes.get(offset..offset + 4)?.try_into().ok()?))
        };
        if word(0)? != MAGIC || word(4)? as usize != bytes.len()
        {
            return None;
        }
        let structure = word(8)? as usize;
        let strings = word(12)? as usize;
        let structure_bytes = word(36)? as usize;
        let strings_bytes = word(32)? as usize;
        if structure.checked_add(structure_bytes)? > bytes.len()
           || strings.checked_add(strings_bytes)? > bytes.len()
        {
            return None;
        }
        Some(Self { structure: &bytes[structure..structure + structure_bytes],
                    strings:   &bytes[strings..strings + strings_bytes] })
    }

    /// The first `reg` window of the node whose `compatible` matches, as
    /// `(address, bytes)`. Absent or malformed entries yield `None` rather than
    /// a guessed address.
    pub fn window(&self, compatible: &str) -> Option<(u64, u64)>
    {
        let mut cursor = 0usize;
        let mut matches = false;
        let mut window = None;
        while cursor + 4 <= self.structure.len()
        {
            let token = self.word(&mut cursor)?;
            match token
            {
                BEGIN_NODE =>
                {
                    self.skip_name(&mut cursor);
                    matches = false;
                    window = None;
                }
                END_NODE =>
                {
                    if matches
                    {
                        if let Some(found) = window
                        {
                            return Some(found);
                        }
                    }
                }
                PROP =>
                {
                    let length = self.word(&mut cursor)? as usize;
                    let name_offset = self.word(&mut cursor)? as usize;
                    let start = cursor;
                    if start + length > self.structure.len()
                    {
                        return None;
                    }
                    let name = self.name(name_offset)?;
                    if name == "compatible"
                    {
                        let value = &self.structure[start..start + length];
                        let end = value.iter().position(|byte| *byte == 0).unwrap_or(length);
                        matches = value.get(..end) == Some(compatible.as_bytes());
                    }
                    else if name == "reg" && length == 16
                    {
                        let cell = |index: usize| -> u64 {
                            u32::from_be_bytes(self.structure[start + index * 4
                                                              ..start + index * 4 + 4]
                                                   .try_into()
                                                   .unwrap()) as u64
                        };
                        window = Some(((cell(0) << 32) | cell(1), (cell(2) << 32) | cell(3)));
                    }
                    self.align(&mut cursor, length);
                }
                NOP => {}
                END => break,
                _ => return None
            }
        }
        None
    }

    fn word(&self, cursor: &mut usize) -> Option<u32>
    {
        let end = cursor.checked_add(4)?;
        if end > self.structure.len()
        {
            return None;
        }
        let value = u32::from_be_bytes(self.structure[*cursor..end].try_into().ok()?);
        *cursor = end;
        Some(value)
    }

    fn skip_name(&self, cursor: &mut usize)
    {
        while *cursor < self.structure.len() && self.structure[*cursor] != 0
        {
            *cursor += 1;
        }
        *cursor += 1;
        while *cursor % 4 != 0
        {
            *cursor += 1;
        }
    }

    fn align(&self, cursor: &mut usize, length: usize)
    {
        *cursor += length;
        while *cursor % 4 != 0
        {
            *cursor += 1;
        }
    }

    fn name(&self, offset: usize) -> Option<&'a str>
    {
        let rest = self.strings.get(offset..)?;
        let end = rest.iter().position(|byte| *byte == 0)?;
        core::str::from_utf8(&rest[..end]).ok()
    }
}
