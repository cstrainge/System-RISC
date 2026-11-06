//! Flattened device tree reader, shared by every bootloader.
//!
//! The tree is the machine's description of itself, and more than one consumer
//! needs to read it: each bootloader finds its console and storage through it,
//! and the kernel learns its peripherals from it. One reader means one answer to
//! "what does this blob say", which is the property that matters - three copies
//! of a parser is three places for tolerance of malformed input to differ
//! silently.
//!
//! The envelope is validated strictly, because a blob that is structurally wrong
//! cannot be read correctly by being lenient: magic, total size, version, block
//! bounds and alignment, the reservation block, and that the blocks do not
//! overlap. What is *not* validated is the vocabulary - unknown properties and
//! nodes are skipped rather than rejected, so a machine's extras are data rather
//! than a reader change. A missing capability is the consumer's error to name,
//! never this reader's reason to refuse the whole tree.
#![no_std]

use core::ops::Range;

const MAGIC: u32 = 0xd00d_feed;
const HEADER_BYTES: usize = 40;
/// The version this reader understands. A blob that requires a newer one is
/// refused rather than guessed at.
const VERSION: u32 = 17;

const BEGIN_NODE: u32 = 1;
const END_NODE: u32 = 2;
const PROP: u32 = 3;
const NOP: u32 = 4;
const END: u32 = 9;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    /// Not a device tree at all.
    Magic,
    /// The header's total size is smaller than a header or larger than the slice.
    Size,
    /// The blob requires a version this reader does not implement.
    Version,
    /// A block's offset or length lies outside the blob.
    Bounds,
    /// A block is not aligned as the specification requires.
    Alignment,
    /// The structure, strings and reservation blocks overlap.
    Overlap,
    /// A reservation entry overflows the address space.
    Reservation,
    /// The structure block ends without its END token, or holds an unknown one.
    Structure,
    /// A string offset does not name a terminated string.
    Strings
}

impl core::fmt::Display for Error
{
    fn fmt(&self, out: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        out.write_str(match self
        {
            Error::Magic =>
            {
                "not a device tree: wrong magic"
            }
            Error::Size =>
            {
                "device tree total size is smaller than its header or larger than the blob"
            }
            Error::Version =>
            {
                "device tree requires a version this reader does not implement"
            }
            Error::Bounds =>
            {
                "device tree block lies outside the blob"
            }
            Error::Alignment =>
            {
                "device tree block is not aligned"
            }
            Error::Overlap =>
            {
                "device tree blocks overlap"
            }
            Error::Reservation =>
            {
                "device tree reservation overflows the address space"
            }
            Error::Structure =>
            {
                "device tree structure block is malformed"
            }
            Error::Strings =>
            {
                "device tree string offset is invalid"
            }
        })
    }
}

/// What one node offers the walk, gathered before the callback runs.
///
/// A node is described by properties that may appear in any order, so the walk
/// collects the ones a consumer can act on and reports them together. Absent
/// properties stay absent: a device with no interrupt is a fact, not a zero.
pub struct Node<'a>
{
    /// The node's own name, e.g. `cpu@1`.
    pub name:         &'a str,
    /// The first entry of `compatible`, if the node declares one.
    pub compatible:   Option<&'a str>,
    /// `device_type`, which is how memory and cpu nodes identify themselves.
    pub device_type:  Option<&'a str>,
    /// The raw `reg` property, unparsed, because its cell layout depends on the
    /// parent's `#address-cells` and `#size-cells`.
    pub reg:          Option<&'a [u8]>,
    /// The raw `interrupts` property, absent when the device raises none.
    pub interrupts:   Option<&'a [u8]>,
    /// `status`, which is how a machine disables a node it describes.
    pub status:       Option<&'a str>,
    /// Whether the node declares `dma-coherent`.
    pub dma_coherent: bool,
    /// Depth below the root, so a consumer can tell `/cpus/cpu@0` from a
    /// same-named node elsewhere.
    pub depth:        usize
}

impl Node<'_>
{
    /// The first two-cell address/size pair of `reg`, when it has one.
    pub fn window(&self) -> Option<(u64, u64)>
    {
        let reg = self.reg?;
        if reg.len() < 16
        {
            return None;
        }
        let cell = |index: usize| -> u64 {
            u32::from_be_bytes(reg[index * 4..index * 4 + 4].try_into().unwrap()) as u64
        };
        Some(((cell(0) << 32) | cell(1), (cell(2) << 32) | cell(3)))
    }

    /// The single-cell value of `reg`, which is how a CPU states its hart id.
    pub fn unit(&self) -> Option<u64>
    {
        let reg = self.reg?;
        if reg.len() != 4
        {
            return None;
        }
        Some(u32::from_be_bytes(reg.try_into().unwrap()) as u64)
    }

    /// Whether this node is enabled. A node with no `status` is enabled; a
    /// machine disables one by stating `disabled`, and anything else is treated
    /// as a status this reader does not know rather than as enabled.
    pub fn enabled(&self) -> bool
    {
        matches!(self.status, None | Some("okay") | Some("ok"))
    }
}

pub struct Tree<'a>
{
    bytes:     &'a [u8],
    structure: &'a [u8],
    strings:   &'a [u8]
}

fn word(bytes: &[u8], offset: usize) -> Result<u32, Error>
{
    let end = offset.checked_add(4).ok_or(Error::Bounds)?;
    let slice = bytes.get(offset..end).ok_or(Error::Bounds)?;
    Ok(u32::from_be_bytes(slice.try_into().unwrap()))
}

fn wide(bytes: &[u8], offset: usize) -> Result<u64, Error>
{
    let high = word(bytes, offset)? as u64;
    let low = word(bytes, offset.checked_add(4).ok_or(Error::Bounds)?)? as u64;
    Ok((high << 32) | low)
}

fn overlap(a: &Range<usize>, b: &Range<usize>) -> bool
{
    a.start < b.end && b.start < a.end
}

fn extent(base: u64, length: u64) -> Result<Range<u64>, Error>
{
    let end = base.checked_add(length).ok_or(Error::Reservation)?;
    Ok(base..end)
}

impl<'a> Tree<'a>
{
    /// Accepts a blob only when its envelope is self-consistent.
    ///
    /// A slice longer than the blob's own total size is accepted and only the
    /// first `total` bytes are used, because a firmware may hand over the whole
    /// reservation it placed the tree in rather than the tree alone. A slice
    /// shorter than the total size is refused: the tree is truncated.
    pub fn new(bytes: &'a [u8]) -> Result<Self, Error>
    {
        if word(bytes, 0)? != MAGIC
        {
            return Err(Error::Magic);
        }
        let total = word(bytes, 4)? as usize;
        if total < HEADER_BYTES || total > bytes.len()
        {
            return Err(Error::Size);
        }
        let bytes = &bytes[..total];
        // `version` must be at least the one we implement, and the last
        // compatible version must not exceed it: a blob that requires a newer
        // reader cannot be read by guessing.
        if word(bytes, 20)? < VERSION || word(bytes, 24)? > VERSION
        {
            return Err(Error::Version);
        }
        let block = |base: usize, length: usize| -> Result<Range<usize>, Error> {
            let end = base.checked_add(length).ok_or(Error::Bounds)?;
            if base < HEADER_BYTES || end > total
            {
                return Err(Error::Bounds);
            }
            Ok(base..end)
        };
        let structure = block(word(bytes, 8)? as usize, word(bytes, 36)? as usize)?;
        let strings = block(word(bytes, 12)? as usize, word(bytes, 32)? as usize)?;
        let reserved_start = word(bytes, 16)? as usize;
        if structure.start % 4 != 0 || reserved_start % 8 != 0 || reserved_start < HEADER_BYTES
        {
            return Err(Error::Alignment);
        }
        let mut end = reserved_start;
        loop
        {
            let base = wide(bytes, end)?;
            let size = wide(bytes, end.checked_add(8).ok_or(Error::Bounds)?)?;
            end = end.checked_add(16).ok_or(Error::Bounds)?;
            if base == 0 && size == 0
            {
                break;
            }
            extent(base, size)?;
        }
        let reserved = reserved_start..end;
        if overlap(&structure, &strings)
           || overlap(&structure, &reserved)
           || overlap(&strings, &reserved)
        {
            return Err(Error::Overlap);
        }
        Ok(Self { bytes,
                  structure: &bytes[structure],
                  strings:   &bytes[strings] })
    }

    /// The blob's own total size, not the length of the slice it was read from.
    pub fn total_size(&self) -> usize
    {
        self.bytes.len()
    }

    /// Visits every node in tree order with the properties a consumer can act
    /// on. Returns how many nodes it saw.
    ///
    /// Properties accumulate per depth. A node is visited when it ends, so
    /// without a stack the last child's properties would leak into every
    /// ancestor's visit and a four-hart machine would report five CPUs.
    pub fn for_each_node(&self, mut visit: impl FnMut(&Node<'_>)) -> Result<usize, Error>
    {
        /// Deep enough for any real tree's device nesting; a tree that exceeds
        /// it is refused rather than silently truncated.
        const MAX_DEPTH: usize = 32;
        #[derive(Clone, Copy, Default)]
        struct State<'a>
        {
            name:         &'a str,
            compatible:   Option<&'a str>,
            device_type:  Option<&'a str>,
            reg:          Option<&'a [u8]>,
            interrupts:   Option<&'a [u8]>,
            status:       Option<&'a str>,
            dma_coherent: bool
        }
        let mut stack = [State::default(); MAX_DEPTH];
        let mut depth = 0usize;
        let mut cursor = 0usize;
        let mut seen = 0usize;
        while cursor + 4 <= self.structure.len()
        {
            let token = self.word(&mut cursor)?;
            match token
            {
                BEGIN_NODE =>
                {
                    if depth >= MAX_DEPTH
                    {
                        return Err(Error::Structure);
                    }
                    let name = self.take_name(&mut cursor)?;
                    stack[depth] = State { name,
                                           ..State::default() };
                    depth += 1;
                }
                END_NODE =>
                {
                    if depth == 0
                    {
                        return Err(Error::Structure);
                    }
                    depth -= 1;
                    let state = stack[depth];
                    visit(&Node { name:         state.name,
                                  compatible:   state.compatible,
                                  device_type:  state.device_type,
                                  reg:          state.reg,
                                  interrupts:   state.interrupts,
                                  status:       state.status,
                                  dma_coherent: state.dma_coherent,
                                  depth });
                    seen += 1;
                }
                PROP =>
                {
                    let length = self.word(&mut cursor)? as usize;
                    let name_offset = self.word(&mut cursor)? as usize;
                    let start = cursor;
                    let end = start.checked_add(length).ok_or(Error::Structure)?;
                    if end > self.structure.len()
                    {
                        return Err(Error::Structure);
                    }
                    let value = &self.structure[start..end];
                    if depth > 0
                    {
                        let property = self.name(name_offset)?;
                        let state = &mut stack[depth - 1];
                        match property
                        {
                            "compatible" =>
                            {
                                state.compatible = Some(first_string(value));
                            }
                            "device_type" =>
                            {
                                state.device_type = Some(first_string(value));
                            }
                            "status" => state.status = Some(first_string(value)),
                            "reg" => state.reg = Some(value),
                            "interrupts" => state.interrupts = Some(value),
                            "dma-coherent" => state.dma_coherent = true,
                            _ => {}
                        }
                    }
                    self.align(&mut cursor, length)?;
                }
                NOP => {}
                END => return Ok(seen),
                _ => return Err(Error::Structure)
            }
        }
        // Running out of structure without an END token means the blob was
        // truncated, which is not the same as an empty tree.
        Err(Error::Structure)
    }

    /// The first `reg` window of the node whose `compatible` names `compatible`.
    ///
    /// A `compatible` property is a list of NUL-separated strings, most specific
    /// first, and a machine's own tree uses the standard vocabulary -
    /// `arm,pl011` rather than a record's `pl011` - so any entry matches rather
    /// than only the first.
    pub fn window(&self, compatible: &str) -> Result<Option<(u64, u64)>, Error>
    {
        let mut found = None;
        self.for_each_node(|node| {
              if found.is_none()
                 && node.compatible == Some(compatible)
                 && node.enabled()
                 && let Some(window) = node.window()
              {
                  found = Some(window);
              }
          })?;
        Ok(found)
    }

    /// Whether any enabled node declares `compatible`, so a consumer can tell a
    /// device the machine does not model from one it models under another name.
    pub fn has_compatible(&self, compatible: &str) -> Result<bool, Error>
    {
        let mut found = false;
        self.for_each_node(|node| {
              if node.compatible == Some(compatible) && node.enabled()
              {
                  found = true;
              }
          })?;
        Ok(found)
    }

    /// The enabled hart ids the machine describes, in the order it lists them.
    ///
    /// A CPU node states its hart id in a one-cell `reg`, which is a different
    /// shape from a device window and is why this does not reuse `window`.
    pub fn cpus(&self, mut visit: impl FnMut(u64)) -> Result<usize, Error>
    {
        let mut count = 0usize;
        self.for_each_node(|node| {
              if node.device_type == Some("cpu")
                 && node.enabled()
                 && let Some(hart) = node.unit()
              {
                  visit(hart);
                  count += 1;
              }
          })?;
        Ok(count)
    }

    /// The RAM windows the machine describes, as `(base, bytes)`.
    pub fn memory(&self, mut visit: impl FnMut(u64, u64)) -> Result<usize, Error>
    {
        let mut count = 0usize;
        self.for_each_node(|node| {
              if node.device_type == Some("memory")
                 && node.enabled()
                 && let Some((base, bytes)) = node.window()
              {
                  visit(base, bytes);
                  count += 1;
              }
          })?;
        Ok(count)
    }

    /// The memory the blob itself asks to be kept clear of, as physical ranges.
    ///
    /// These are not reservations the loader made; they are the machine saying
    /// "this memory is spoken for", and a loader that ignores them can hand a
    /// page pool memory that holds firmware state.
    pub fn reservations(&self, mut visit: impl FnMut(Range<u64>)) -> Result<(), Error>
    {
        // Re-read the reservation block from the blob: `new` validated it, so the
        // walk here cannot run off the end.
        let reserved_start = word(self.bytes, 16)? as usize;
        let mut end = reserved_start;
        loop
        {
            let base = wide(self.bytes, end)?;
            let size = wide(self.bytes, end.checked_add(8).ok_or(Error::Bounds)?)?;
            end = end.checked_add(16).ok_or(Error::Bounds)?;
            if base == 0 && size == 0
            {
                return Ok(());
            }
            visit(extent(base, size)?);
        }
    }

    fn word(&self, cursor: &mut usize) -> Result<u32, Error>
    {
        let value = word(self.structure, *cursor)?;
        *cursor += 4;
        Ok(value)
    }

    /// Reads a node name and advances past its alignment padding.
    fn take_name(&self, cursor: &mut usize) -> Result<&'a str, Error>
    {
        let rest = self.structure.get(*cursor..).ok_or(Error::Structure)?;
        let end = rest.iter().position(|byte| *byte == 0).ok_or(Error::Structure)?;
        let name = core::str::from_utf8(&rest[..end]).map_err(|_| Error::Structure)?;
        *cursor += end + 1;
        while *cursor % 4 != 0
        {
            *cursor += 1;
        }
        Ok(name)
    }

    fn align(&self, cursor: &mut usize, length: usize) -> Result<(), Error>
    {
        *cursor = cursor.checked_add(length).ok_or(Error::Structure)?;
        while *cursor % 4 != 0
        {
            *cursor += 1;
        }
        Ok(())
    }

    fn name(&self, offset: usize) -> Result<&'a str, Error>
    {
        let rest = self.strings.get(offset..).ok_or(Error::Strings)?;
        let end = rest.iter().position(|byte| *byte == 0).ok_or(Error::Strings)?;
        core::str::from_utf8(&rest[..end]).map_err(|_| Error::Strings)
    }
}

/// The first NUL-terminated string of a string list, which is the most specific
/// entry of a `compatible` property.
fn first_string(value: &[u8]) -> &str
{
    let end = value.iter().position(|byte| *byte == 0).unwrap_or(value.len());
    core::str::from_utf8(&value[..end]).unwrap_or_default()
}
