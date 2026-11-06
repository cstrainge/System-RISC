//! Read-only walk of the generated device tree.
//!
//! The bootloader consumes the tree rather than knowing peripheral addresses as
//! constants, so the platform record stays the only place a board's hardware is
//! written down. This is deliberately a small reader: it answers "where is the
//! node with this compatible string?" and nothing else. Unknown properties and
//! nodes are skipped rather than rejected, so a future platform's extras are
//! data rather than a bootloader change.

/// Whether a `compatible` property names `wanted`.
///
/// The property is a list of NUL-separated strings and a node may carry several,
/// most specific first. A machine's own tree uses the standard names —
/// `arm,pl011` rather than a record's `pl011` — so a consumer asks in that
/// vocabulary and this matches any entry rather than only the first.
fn compatible_matches(value: &[u8], wanted: &str) -> bool
{
    value.split(|byte| *byte == 0)
         .any(|entry| entry == wanted.as_bytes())
}

/// What one node offers the walk, gathered before the callback runs.
///
/// A node is described by properties that may appear in any order, so the walk
/// collects the ones a consumer can act on and reports them together. Absent
/// properties stay absent: a device with no interrupt is a fact, not a zero.
pub struct Node<'a>
{
    /// The node's own name, e.g. `cpu@1`.
    pub name:       &'a str,
    /// The first entry of `compatible`, if the node declares one.
    pub compatible: Option<&'a str>,
    /// `device_type`, which is how memory and cpu nodes identify themselves.
    pub device_type: Option<&'a str>,
    /// The raw `reg` property, unparsed, because its cell layout depends on the
    /// parent's `#address-cells` and `#size-cells`.
    pub reg:        Option<&'a [u8]>,
    /// The raw `interrupts` property, absent when the device raises none.
    pub interrupts: Option<&'a [u8]>,
    /// Whether the node declares `dma-coherent`.
    pub dma_coherent: bool,
    /// Depth below the root, so a consumer can tell `/cpus/cpu@0` from a
    /// same-named node elsewhere.
    pub depth:      usize
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
}

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
        let mut found = None;
        self.for_each_node(|node| {
             if found.is_none() && node.compatible == Some(compatible)
             {
                 found = node.window();
             }
         });
        found
    }

    /// Whether any node declares `compatible`, so a cross-check can tell a
    /// device the machine does not model from one it models under another name.
    pub fn has_compatible(&self, compatible: &str) -> bool
    {
        let mut found = false;
        self.for_each_node(|node| {
             if node.compatible == Some(compatible)
             {
                 found = true;
             }
         });
        found
    }

    /// The enabled hart ids the machine describes, in the order it lists them.
    ///
    /// A CPU node states its hart id in a one-cell `reg`, which is a different
    /// shape from a device window and is why this does not reuse `window`.
    pub fn cpus(&self, mut visit: impl FnMut(u64)) -> usize
    {
        let mut count = 0usize;
        self.for_each_node(|node| {
             if node.device_type == Some("cpu")
                && let Some(hart) = node.unit()
             {
                 visit(hart);
                 count += 1;
             }
         });
        count
    }

    /// The RAM windows the machine describes, as `(base, bytes)`.
    pub fn memory(&self, mut visit: impl FnMut(u64, u64)) -> usize
    {
        let mut count = 0usize;
        self.for_each_node(|node| {
             if node.device_type == Some("memory")
                && let Some((base, bytes)) = node.window()
             {
                 visit(base, bytes);
                 count += 1;
             }
         });
        count
    }

    /// Visits every node in tree order with the properties a consumer can act
    /// on. Returns how many nodes it saw.
    ///
    /// This is the one walk the specific queries are built on: a machine that
    /// models one node per device and one that models a single bank are both
    /// answered here, and nothing assumes how a machine lays its devices out.
    ///
    /// Properties accumulate per depth. A node is visited when it ends, so
    /// without a stack the last child's properties would leak into every
    /// ancestor's visit and a four-hart machine would report five CPUs — which
    /// is exactly what an earlier version did.
    pub fn for_each_node(&self, mut visit: impl FnMut(&Node<'_>)) -> usize
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
            dma_coherent: bool
        }
        let mut stack = [State::default(); MAX_DEPTH];
        let mut depth = 0usize;
        let mut cursor = 0usize;
        let mut seen = 0usize;
        while cursor + 4 <= self.structure.len()
        {
            let Some(token) = self.word(&mut cursor)
            else
            {
                return seen;
            };
            match token
            {
                BEGIN_NODE =>
                {
                    if depth >= MAX_DEPTH
                    {
                        return seen;
                    }
                    let name = self.take_name(&mut cursor).unwrap_or("");
                    stack[depth] = State { name,
                                           ..State::default() };
                    depth += 1;
                }
                END_NODE =>
                {
                    if depth == 0
                    {
                        continue;
                    }
                    depth -= 1;
                    let state = stack[depth];
                    visit(&Node { name:         state.name,
                                  compatible:   state.compatible,
                                  device_type:  state.device_type,
                                  reg:          state.reg,
                                  interrupts:   state.interrupts,
                                  dma_coherent: state.dma_coherent,
                                  depth });
                    seen += 1;
                }
                PROP =>
                {
                    let Some(length) = self.word(&mut cursor).map(|word| word as usize)
                    else
                    {
                        return seen;
                    };
                    let Some(name_offset) = self.word(&mut cursor).map(|word| word as usize)
                    else
                    {
                        return seen;
                    };
                    let start = cursor;
                    if start + length > self.structure.len()
                    {
                        return seen;
                    }
                    let value = &self.structure[start..start + length];
                    if depth > 0
                       && let Some(property) = self.name(name_offset)
                    {
                        let state = &mut stack[depth - 1];
                        match property
                        {
                            "compatible" =>
                            {
                                let end =
                                    value.iter().position(|byte| *byte == 0).unwrap_or(length);
                                state.compatible = core::str::from_utf8(&value[..end]).ok();
                            }
                            "device_type" =>
                            {
                                let end =
                                    value.iter().position(|byte| *byte == 0).unwrap_or(length);
                                state.device_type = core::str::from_utf8(&value[..end]).ok();
                            }
                            "reg" => state.reg = Some(value),
                            "interrupts" => state.interrupts = Some(value),
                            "dma-coherent" => state.dma_coherent = true,
                            _ => {}
                        }
                    }
                    self.align(&mut cursor, length);
                }
                NOP => {}
                END => break,
                _ => return seen
            }
        }
        seen
    }

    /// Visits every node that carries both a `compatible` and a `reg` window, in
    /// tree order, as `(compatible, address, bytes)`, and returns how many it
    /// saw.
    ///
    /// This is the general form of [`Self::window`]: a machine that models one
    /// node per device and a machine that models a single bank are both answered
    /// by the same walk, so nothing here assumes how a machine lays its devices
    /// out.
    pub fn visit_nodes(&self, mut visit: impl FnMut(&str, u64, u64)) -> usize
    {
        self.for_each_node(|node| {
             if let (Some(compatible), Some((address, bytes))) = (node.compatible, node.window())
             {
                 visit(compatible, address, bytes);
             }
         })
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

    /// Reads a node name and advances past its alignment padding.
    fn take_name(&self, cursor: &mut usize) -> Option<&'a str>
    {
        let start = *cursor;
        while *cursor < self.structure.len() && self.structure[*cursor] != 0
        {
            *cursor += 1;
        }
        if *cursor >= self.structure.len()
        {
            return None;
        }
        let name = core::str::from_utf8(&self.structure[start..*cursor]).ok()?;
        *cursor += 1;
        while *cursor % 4 != 0
        {
            *cursor += 1;
        }
        Some(name)
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
