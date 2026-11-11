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
    Strings,
    /// A cell-count declaration or a `reg` is not decodable as declared.
    Cells,
    /// An address is not covered by any entry of its parent's `ranges`, so
    /// there is no address for it in the parent's parent's space.
    Ranges,
    /// A CPU node is not admissible: missing device_type, wrong reg width, or a
    /// container that does not declare one or two address cells and zero size
    /// cells.
    Cpu,
    /// A `/reserved-memory` child asks for dynamic allocation, which this reader
    /// refuses rather than ignoring.
    DynamicReservation
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
            Error::Cells =>
            {
                "device tree cell declaration or reg is not decodable as declared"
            }
            Error::Ranges =>
            {
                "device tree address is not covered by its parent's ranges"
            }
            Error::Cpu =>
            {
                "device tree CPU node is not admissible"
            }
            Error::DynamicReservation =>
            {
                "device tree reserved-memory child requests dynamic allocation"
            }
        })
    }
}

/// What one node offers the walk, gathered before the callback runs.
///
/// A node is described by properties that may appear in any order, so the walk
/// collects the ones a consumer can act on and reports them together. Absent
/// properties stay absent: a device with no interrupt is a fact, not a zero.
#[derive(Clone, Copy)]
pub struct Node<'a>
{
    /// The node's own name, e.g. `cpu@1`.
    pub name:         &'a str,
    /// The parent's name, or `None` at the root. A child is visited before its
    /// parent, so this is how a consumer knows which container it is in.
    ///
    /// A name is not an identity: two siblings may share one. Use `index` when
    /// the answer must be about a particular node.
    pub parent:       Option<&'a str>,
    /// This node's position in the walk, unique within the tree.
    ///
    /// A node-at-END walk cannot otherwise express instance identity, so a
    /// consumer that needs to know whether *this* node has children, or that
    /// two same-named nodes are different, would have to reach past the API into
    /// the token stream. `index` exists so it does not have to.
    pub index:        usize,
    /// The index of this node's parent, or `None` at the root. Leaf-ness is
    /// "no node has `parent_index == Some(this.index)`".
    pub parent_index: Option<usize>,
    /// The first entry of `compatible`, if the node declares one.
    pub compatible:   Option<&'a str>,
    /// The whole `compatible` list, so a consumer can match any entry rather
    /// than only the most specific one.
    pub compatible_list: Option<&'a [u8]>,
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
    pub depth:        usize,
    /// The parent's `#address-cells`, which is how wide each address is in this
    /// node's `reg`. Inherited, because a `reg` cannot be decoded without it.
    pub address_cells: Option<u32>,
    /// The parent's `#size-cells`, likewise for the length half of `reg`.
    pub size_cells:    Option<u32>,
    /// What this node itself declares, which is what its children inherit. A
    /// container is validated on these, a leaf on the inherited pair.
    pub own_address_cells: Option<u32>,
    pub own_size_cells:    Option<u32>,
    /// The raw `ranges` property. Empty means an identity mapping, which is what
    /// a consumer that supports one translation only needs to know.
    pub ranges:       Option<&'a [u8]>,
    /// The raw `interrupts-extended` property, absent when the node uses the
    /// phandle-based `interrupts` form instead.
    pub interrupts_extended: Option<&'a [u8]>,
    /// `phandle`, if the node declares one.
    pub phandle:      Option<u32>,
    /// `linux,phandle`, the legacy spelling of the same thing.
    pub linux_phandle: Option<u32>,
    /// Whether the node declares the `interrupt-controller` flag. Presence is the
    /// fact; the property has no value.
    pub interrupt_controller: bool,
    /// `#interrupt-cells`, how wide each interrupt specifier is.
    pub interrupt_cells: Option<u32>,
    /// `interrupt-parent`, the phandle of the controller this node's
    /// `interrupts` specifiers are written for.
    ///
    /// Without it the width of a specifier is unknown, and a consumer can only
    /// guess by length. The property is inherited from the nearest ancestor that
    /// states one, so a consumer resolves it by walking `parent_index`.
    pub interrupt_parent: Option<u32>,
    /// `timebase-frequency`, the counter rate a machine states for itself.
    pub timebase_frequency: Option<u32>,
    /// `method`, which is how a firmware-interface node states the conduit its
    /// calls travel over. The PSCI binding defines two: `hvc` and `smc`.
    ///
    /// A node that describes a function rather than a register window has no
    /// `reg`, so this is the property that makes its declaration concrete. The
    /// reader reports the spelling the machine wrote; whether a consumer
    /// implements it is the consumer's answer, not this reader's.
    pub method:           Option<&'a str>
}

impl Node<'_>
{
    /// The first address/size pair of `reg`, decoded as the parent's cell
    /// declarations say it is.
    ///
    /// The address is in the parent's address space, not the CPU's: a bus
    /// that remaps its children needs `Tree::window`, which translates
    /// through the ancestors' `ranges`. A `reg` whose cell widths are not
    /// one or two has no pair here rather than a guessed one.
    pub fn window(&self) -> Option<(u64, u64)>
    {
        let reg = self.reg?;
        let address = self.address_cells.unwrap_or(2);
        let size = self.size_cells.unwrap_or(1);
        let stride = (address as usize + size as usize) * 4;
        if stride == 0 || reg.len() < stride
        {
            return None;
        }
        let base = cells(reg, address).ok()?;
        let length = cells(&reg[address as usize * 4..], size).ok()?;
        Some((base, length))
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

    /// Whether this node's `compatible` list names `compatible`.
    ///
    /// Any entry matches, not only the most specific one, because a machine's
    /// tree uses the standard vocabulary (`arm,pl011` rather than a record's
    /// `pl011`) and a consumer that matched only the first entry would read a
    /// different answer from the same tree. This is the same predicate the
    /// class walks use, so a consumer filtering for itself selects what they
    /// select.
    pub fn names(&self, compatible: &str) -> bool
    {
        names(self, compatible)
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
    pub fn for_each_node(&self, mut visit: impl FnMut(&Node<'a>)) -> Result<usize, Error>    {
        /// Deep enough for any real tree's device nesting; a tree that exceeds
        /// it is refused rather than silently truncated.
        const MAX_DEPTH: usize = 32;
        #[derive(Clone, Copy, Default)]
        struct State<'a>
        {
            name:         &'a str,
            parent:       Option<&'a str>,
            index:        usize,
            parent_index: Option<usize>,
            compatible:   Option<&'a str>,
            compatible_list: Option<&'a [u8]>,
            device_type:  Option<&'a str>,
            reg:          Option<&'a [u8]>,
            interrupts:   Option<&'a [u8]>,
            interrupts_extended: Option<&'a [u8]>,
            ranges:       Option<&'a [u8]>,
            status:       Option<&'a str>,
            dma_coherent: bool,
            interrupt_controller: bool,
            phandle:      Option<u32>,
            linux_phandle: Option<u32>,
            interrupt_cells: Option<u32>,
            interrupt_parent: Option<u32>,
            timebase_frequency: Option<u32>,
            method:       Option<&'a str>,
            address_cells: Option<u32>,
            size_cells:    Option<u32>,
            own_address_cells: Option<u32>,
            own_size_cells:    Option<u32>,
            /// Whether a child has been opened. The specification requires a
            /// node's properties to precede its subnodes, and a property after a
            /// child cannot be attributed to the node it appears to belong to
            /// without guessing.
            children_started: bool
        }
        let mut stack = [State::default(); MAX_DEPTH];
        let mut depth = 0usize;
        let mut cursor = 0usize;
        let mut seen = 0usize;
        let mut next_index = 0usize;
        let mut root_started = false;
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
                    // A blob has exactly one root, and the root is unnamed.
                    if depth == 0
                    {
                        if root_started || !name.is_empty()
                        {
                            return Err(Error::Structure);
                        }
                        root_started = true;
                    }
                    // A node inherits its parent's cell declarations; the root
                    // has none until it declares them, and the specification's
                    // default for the root is two address cells and one size.
                    let (address_cells, size_cells) = if depth == 0
                    {
                        (Some(2), Some(1))
                    }
                    else
                    {
                        // The parent's own declaration is what its children
                        // inherit; its inherited pair describes the parent.
                        let parent = &stack[depth - 1];
                        (parent.own_address_cells.or(parent.address_cells),
                         parent.own_size_cells.or(parent.size_cells))
                    };
                    let index = next_index;
                    next_index += 1;
                    // Opening a child means the parent's property block is over.
                    if depth > 0
                    {
                        stack[depth - 1].children_started = true;
                    }
                    stack[depth] = State { name,
                                           parent: if depth == 0
                                           {
                                               None
                                           }
                                           else
                                           {
                                               Some(stack[depth - 1].name)
                                           },
                                           index,
                                           parent_index: if depth == 0
                                           {
                                               None
                                           }
                                           else
                                           {
                                               Some(stack[depth - 1].index)
                                           },
                                           address_cells,
                                           size_cells,
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
                                  parent:       state.parent,
                                  index:        state.index,
                                  parent_index: state.parent_index,
                                  compatible:   state.compatible,
                                  compatible_list: state.compatible_list,
                                  device_type:  state.device_type,
                                  reg:          state.reg,
                                  interrupts:   state.interrupts,
                                  status:       state.status,
                                  dma_coherent: state.dma_coherent,
                                  depth,
                                  address_cells: state.address_cells,
                                  size_cells: state.size_cells,
                                  own_address_cells: state.own_address_cells,
                                  own_size_cells: state.own_size_cells,
                                  ranges: state.ranges,
                                  interrupts_extended: state.interrupts_extended,
                                  phandle: state.phandle,
                                  linux_phandle: state.linux_phandle,
                                  interrupt_controller: state.interrupt_controller,
                                  interrupt_cells: state.interrupt_cells,
                                  interrupt_parent: state.interrupt_parent,
                                  timebase_frequency: state.timebase_frequency,
                                  method:       state.method });
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
                        // The specification requires a node's properties to
                        // precede its subnodes. A property after a child cannot
                        // be attributed to the node it appears to belong to
                        // without guessing, so the tree is refused instead.
                        if state.children_started
                        {
                            return Err(Error::Structure);
                        }
                        match property
                        {
                            "compatible" =>
                            {
                                state.compatible = Some(first_string(value));
                                state.compatible_list = Some(value);
                            }
                            "device_type" =>
                            {
                                state.device_type = Some(first_string(value));
                            }
                            "status" => state.status = Some(first_string(value)),
                            "reg" => state.reg = Some(value),
                            "interrupts" => state.interrupts = Some(value),
                            "interrupts-extended" => state.interrupts_extended = Some(value),
                            "ranges" => state.ranges = Some(value),
                            "dma-coherent" => state.dma_coherent = true,
                            "interrupt-controller" => state.interrupt_controller = true,
                            "phandle" => state.phandle = cell_count(value),
                            "linux,phandle" => state.linux_phandle = cell_count(value),
                            "#interrupt-cells" => state.interrupt_cells = cell_count(value),
                            "interrupt-parent" => state.interrupt_parent = cell_count(value),
                            "timebase-frequency" => state.timebase_frequency = cell_count(value),
                            "method" => state.method = Some(first_string(value)),
                            "#address-cells" =>
                            {
                                state.own_address_cells = cell_count(value);
                            }
                            "#size-cells" =>
                            {
                                state.own_size_cells = cell_count(value);
                            }
                            _ => {}
                        }
                    }
                    self.align(&mut cursor, length)?;
                }
                NOP => {}
                END =>
                {
                    // Trailing bytes after the END token are not part of the
                    // tree; only alignment padding may follow.
                    if self.structure[cursor..].iter().any(|byte| *byte != 0)
                    {
                        return Err(Error::Structure);
                    }
                    return Ok(seen);
                }
                _ => return Err(Error::Structure)
            }
        }
        // Running out of structure without an END token means the blob was
        // truncated, which is not the same as an empty tree.
        Err(Error::Structure)
    }

    /// The first node the walk accepts, so a consumer can resolve a phandle or
    /// an index without reaching past the API into the token stream.
    pub fn find(&self, mut accept: impl FnMut(&Node<'a>) -> bool) -> Result<Option<Node<'a>>, Error>
    {
        let mut found = None;
        self.for_each_node(|node| {
                if found.is_none() && accept(node)
                {
                    found = Some(*node);
                }
            })?;
        Ok(found)
    }

    /// The first `reg` window of the node whose `compatible` names `compatible`.
    ///
    /// A `compatible` property is a list of NUL-separated strings, most specific
    /// first, and a machine's own tree uses the standard vocabulary -
    /// `arm,pl011` rather than a record's `pl011` - so any entry matches rather
    /// than only the first. The address is translated to the CPU's address
    /// space through the ancestors' `ranges`; a node under a bus that remaps
    /// its children resolves to where the CPU actually finds it.
    pub fn window(&self, compatible: &str) -> Result<Option<(u64, u64)>, Error>
    {
        match self.find_compatible(compatible)?
        {
            Some(node) => self.window_of(node),
            None => Ok(None)
        }
    }

    /// The first `reg` window of the node whose `compatible` names `compatible`,
    /// whether or not the node is enabled.
    ///
    /// The one place a consumer goes when the node it needs is disabled in the
    /// tree for a reason that does not apply to it: the bootrom reading its own
    /// boot device is not a Linux driver-binding question, so a `status` of
    /// `disabled` on that node must not hide the device the machine boots from.
    /// `find_compatible` keeps its enabled-only meaning for every other
    /// consumer; this exception is explicit and local to the call.
    pub fn window_including_disabled(&self, compatible: &str) -> Result<Option<(u64, u64)>, Error>
    {
        match self.find_compatible_including_disabled(compatible)?
        {
            Some(node) => self.window_of(node),
            None => Ok(None)
        }
    }

    /// The first node whose `compatible` list names `compatible`, whether or
    /// not it is enabled.
    ///
    /// `find_compatible` answers "the device the machine offers"; this answers
    /// "the device the machine describes", which is the question when a node's
    /// `status` speaks about driver binding rather than about the hardware's
    /// existence.
    pub fn find_compatible_including_disabled(&self, compatible: &str)
                                              -> Result<Option<Node<'a>>, Error>
    {
        self.find(|node| names(node, compatible))
    }

    /// The first `reg` window of `node`, translated to the CPU's address space.
    fn window_of(&self, node: Node<'a>) -> Result<Option<(u64, u64)>, Error>
    {
        let Some((base, bytes)) = node.window()
        else
        {
            return Ok(None);
        };
        Ok(Some((self.translate(&node, base)?, bytes)))
    }

    /// Translates `address`, which names a window in `node`'s parent's address
    /// space, up to the CPU's address space by walking the ancestors and
    /// applying each one's `ranges`.
    ///
    /// The walk is one `find` per level, which is a full pass over the tree
    /// each time; a bootloader resolves a handful of windows once, so the
    /// passes are cheaper than the bookkeeping a single-pass chain would need.
    fn translate(&self, node: &Node<'a>, mut address: u64) -> Result<u64, Error>
    {
        let mut current = *node;
        while let Some(parent_index) = current.parent_index
        {
            let Some(parent) = self.find(|candidate| candidate.index == parent_index)?
            else
            {
                return Err(Error::Structure);
            };
            address = apply_ranges(&parent, address)?;
            current = parent;
        }
        Ok(address)
    }

    /// The first enabled node whose `compatible` list names `compatible`.
    ///
    /// The one place that decides which node an identity names, so a consumer
    /// that needs more than the window - an interrupt, a DMA attribute - selects
    /// the same node the window came from.
    pub fn find_compatible(&self, compatible: &str) -> Result<Option<Node<'a>>, Error>
    {
        self.find(|node| node.enabled() && names(node, compatible))
    }

    /// Every enabled node whose `compatible` list names `compatible`, in tree
    /// order, and how many there were.
    ///
    /// A class of device is often several: a machine describes as many windows
    /// of a kind as it has, and the count is the machine's fact rather than
    /// something a consumer may bound. `find_compatible` answers "the one", so
    /// enumerating a plural device needs this instead.
    pub fn for_each_compatible(&self, compatible: &str,
                               mut visit: impl FnMut(&Node<'a>))
                               -> Result<usize, Error>
    {
        let mut count = 0;
        self.for_each_node(|node| {
                if node.enabled() && names(node, compatible)
                {
                    visit(&node);
                    count += 1;
                }
            })?;
        Ok(count)
    }

    /// Whether any enabled node declares `compatible`, so a consumer can tell a
    /// device the machine does not model from one it models under another name.
    pub fn has_compatible(&self, compatible: &str) -> Result<bool, Error>
    {
        let mut found = false;
        self.for_each_node(|node| {
              if names(&node, compatible) && node.enabled()
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


    /// The RAM windows the machine describes, decoded as its cell declarations
    /// say they are.
    ///
    /// `reg` is only decodable together with the parent's `#address-cells` and
    /// `#size-cells`, so a root that declares something other than the
    /// specification's default is honoured rather than assumed, and a bank whose
    /// `reg` does not divide into whole address/size pairs is refused instead of
    /// half-read.
    pub fn memory_checked(&self, mut visit: impl FnMut(u64, u64)) -> Result<usize, Error>
    {
        let mut count = 0usize;
        let mut problem: Option<Error> = None;
        self.for_each_node(|node| {
              if problem.is_some() || node.device_type != Some("memory") || !node.enabled()
              {
                  return;
              }
              let Some(reg) = node.reg
              else
              {
                  return;
              };
              let address = node.address_cells.unwrap_or(2);
              let size = node.size_cells.unwrap_or(1);
              let stride = (address as usize + size as usize) * 4;
              if stride == 0 || reg.len() < stride || reg.len() % stride != 0
              {
                  problem = Some(Error::Cells);
                  return;
              }
              let base = cells(reg, address).unwrap_or(0);
              let length = cells(&reg[address as usize * 4..], size).unwrap_or(0);
              visit(base, length);
              count += 1;
          })?;
        match problem
        {
            Some(error) => Err(error),
            None => Ok(count)
        }
    }

    /// The enabled hart ids the machine describes, with the CPU container's
    /// declarations checked.
    ///
    /// A CPU node's `reg` holds exactly one hart id, whose width the `/cpus`
    /// container declares; a node that disagrees with its container is refused
    /// rather than decoded by guessing. The status of a *disabled* CPU is
    /// checked too, so a machine that writes a status this reader does not know
    /// is refused instead of silently dropping a hart.
    pub fn cpus_checked(&self, mut visit: impl FnMut(u64)) -> Result<usize, Error>
    {
        let mut count = 0usize;
        let mut seen_container = false;
        let mut problem: Option<Error> = None;
        self.for_each_node(|node| {
              if problem.is_some()
              {
                  return;
              }
              let is_container = node.depth == 1 && node.name == "cpus";
              if is_container
              {
                  if seen_container
                  {
                      problem = Some(Error::Cpu);
                      return;
                  }
                  seen_container = true;
                  // `/cpus` must declare one or two address cells and no size
                  // cells: a CPU has an address, not an extent.
                  match (node.own_address_cells, node.own_size_cells)
                  {
                      (Some(1 | 2), Some(0)) => {}
                      _ => problem = Some(Error::Cpu)
                  }
                  return;
              }
              if node.depth != 2 || node.parent != Some("cpus")
              {
                  return;
              }
              let named = node.name == "cpu" || node.name.starts_with("cpu@");
              if !named && node.device_type != Some("cpu")
              {
                  return;
              }
              if node.device_type != Some("cpu")
              {
                  problem = Some(Error::Cpu);
                  return;
              }
              let Some(reg) = node.reg
              else
              {
                  problem = Some(Error::Cpu);
                  return;
              };
              let address = match node.address_cells
              {
                  Some(1 | 2) => node.address_cells.unwrap(),
                  _ => {
                      problem = Some(Error::Cpu);
                      return;
                  }
              };
              if node.size_cells != Some(0) || reg.len() != address as usize * 4
              {
                  problem = Some(Error::Cpu);
                  return;
              }
              // An unknown status is refused rather than treated as enabled or
              // as silently absent.
              let enabled = match node.status
              {
                  None | Some("okay") | Some("ok") => true,
                  Some("disabled") | Some("reserved") | Some("fail") => false,
                  Some(other) if other.starts_with("fail-") => false,
                  Some(_) =>
                  {
                      problem = Some(Error::Cpu);
                      return;
                  }
              };
              if enabled
              {
                  visit(cells(reg, address).unwrap_or(0));
                  count += 1;
              }
          })?;
        match problem
        {
            Some(error) => Err(error),
            None => Ok(count)
        }
    }

    /// Every extent the machine asks to be kept clear of: its reservation block
    /// and the static children of `/reserved-memory`.
    ///
    /// A `/reserved-memory` child that asks for dynamic allocation is refused
    /// rather than ignored, because ignoring it would hand the page pool memory
    /// the machine reserved for itself.
    pub fn reservations_checked(&self, mut visit: impl FnMut(Range<u64>) -> Result<(), Error>)
                                -> Result<(), Error>
    {
        // The reservation block first, then the static reserved-memory children.
        // A visitor that refuses must not have its refusal swallowed: the
        // reservation block is walked by a closure that cannot return an error,
        // so the first refusal is carried out by hand.
        let mut refused: Option<Error> = None;
        self.reservations(|range| {
                  if refused.is_none()
                     && let Err(error) = visit(range)
                  {
                      refused = Some(error);
                  }
              })?;
        if let Some(error) = refused
        {
            return Err(error);
        }
        let mut problem: Option<Error> = None;
        self.for_each_node(|node| {
              if problem.is_some()
              {
                  return;
              }
              if node.depth != 2 || node.parent != Some("reserved-memory") || !node.enabled()
              {
                  return;
              }
              if node.name == "ranges" || node.reg.is_none()
              {
                  // A child with no `reg` is not describing a static extent;
                  // one that asks for `size` wants a dynamic allocation.
                  if node.reg.is_none()
                  {
                      problem = Some(Error::DynamicReservation);
                  }
                  return;
              }
              let address = node.address_cells.unwrap_or(2);
              let size = node.size_cells.unwrap_or(1);
              let stride = (address as usize + size as usize) * 4;
              let reg = node.reg.unwrap();
              if stride == 0 || reg.len() < stride || reg.len() % stride != 0
              {
                  problem = Some(Error::Cells);
                  return;
              }
              let base = cells(reg, address).unwrap_or(0);
              let length = cells(&reg[address as usize * 4..], size).unwrap_or(0);
              match extent(base, length)
              {
                  Ok(range) =>
                  {
                      if let Err(error) = visit(range)
                      {
                          problem = Some(error);
                      }
                  }
                  Err(error) =>
                  {
                      problem = Some(error);
                  }
              }
          })?;
        match problem
        {
            Some(error) => Err(error),
            None => Ok(())
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

/// Whether a node's `compatible` list names `wanted`.
///
/// The property is a list of NUL-separated strings, most specific first, and a
/// machine's own tree uses the standard vocabulary - `arm,pl011` rather than a
/// record's `pl011` - so any entry matches rather than only the first.
fn names(node: &Node<'_>, wanted: &str) -> bool
{
    let Some(list) = node.compatible_list
    else
    {
        return false;
    };
    list.split(|byte| *byte == 0)
        .any(|entry| entry == wanted.as_bytes())
}

/// A cell-count property's value, which must be exactly one u32.
fn cell_count(value: &[u8]) -> Option<u32>
{
    if value.len() != 4
    {
        return None;
    }
    Some(u32::from_be_bytes(value.try_into().unwrap()))
}

/// Decodes a `reg` address from `count` cells.
fn cells(bytes: &[u8], count: u32) -> Result<u64, Error>
{
    match count
    {
        1 => Ok(word(bytes, 0)? as u64),
        2 => wide(bytes, 0),
        _ => Err(Error::Cells)
    }
}

/// Applies one ancestor's `ranges` to an address in that ancestor's space.
///
/// A `ranges` entry is (child address, parent address, length), where the
/// child address is as wide as the ancestor's own `#address-cells`, the
/// parent address as wide as the address cells the ancestor inherited, and
/// the length as wide as the ancestor's own `#size-cells`. An absent or empty
/// `ranges` is the identity: the child and parent share one address space.
/// An address no entry covers is refused rather than passed through
/// untranslated, because an untranslated bus address would be probed as if
/// it were a CPU address.
fn apply_ranges(ancestor: &Node<'_>, address: u64) -> Result<u64, Error>
{
    let Some(ranges) = ancestor.ranges
    else
    {
        return Ok(address);
    };
    if ranges.is_empty()
    {
        return Ok(address);
    }
    let child_cells = ancestor.own_address_cells.or(ancestor.address_cells).unwrap_or(2);
    let parent_cells = ancestor.address_cells.unwrap_or(2);
    let size_cells = ancestor.own_size_cells.or(ancestor.size_cells).unwrap_or(1);
    let stride = (child_cells as usize + parent_cells as usize + size_cells as usize) * 4;
    if stride == 0 || ranges.len() % stride != 0
    {
        return Err(Error::Cells);
    }
    let mut at = 0usize;
    while at < ranges.len()
    {
        let entry = &ranges[at..at + stride];
        let child = cells(entry, child_cells)?;
        let parent = cells(&entry[child_cells as usize * 4..], parent_cells)?;
        let length =
            cells(&entry[(child_cells as usize + parent_cells as usize) * 4..], size_cells)?;
        if let Some(offset) = address.checked_sub(child)
           && offset < length
        {
            return parent.checked_add(offset).ok_or(Error::Cells);
        }
        at += stride;
    }
    Err(Error::Ranges)
}

/// The first NUL-terminated string of a string list, which is the most specific
/// entry of a `compatible` property.
fn first_string(value: &[u8]) -> &str
{
    let end = value.iter().position(|byte| *byte == 0).unwrap_or(value.len());
    core::str::from_utf8(&value[..end]).unwrap_or_default()
}

