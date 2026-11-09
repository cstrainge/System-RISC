//! Flattened device tree emitted from a platform record.
//!
//! The record is the single source of truth: the bootloader and the kernel are
//! both consumers of this blob, so neither hardcodes hardware the record already
//! declares. Emitting it at build time means a record edit changes both
//! consumers at once, and a broken generator fails the bootloader immediately
//! rather than presenting as "no devices found" later.
//!
//! The blob follows the Devicetree Specification: a 40-byte header, a memory
//! reservation block, a structure block and a strings block. Property and node
//! order is stable so the emitted bytes are reproducible.
use crate::{Result,
            platform::{self, Platform, hardware::Disposition}};

const MAGIC: u32 = 0xd00d_feed;
const VERSION: u32 = 17;
const LAST_COMPATIBLE_VERSION: u32 = 16;
const HEADER_BYTES: usize = 40;

// Devicetree Specification token values. These are written as literals in the
// test parser below rather than referenced, so a wrong constant here cannot hide
// behind the parser agreeing with it.
const BEGIN_NODE: u32 = 1;
const END_NODE: u32 = 2;
const PROP: u32 = 3;
const END: u32 = 9;

/// Whether this record says its machine description is built from the record.
///
/// Two kinds of record generate a tree and neither may be inferred from the
/// other. A record with an emulation section describes the machine the emulator
/// models, and states the fact there because it is about that machine. A board
/// describes itself: it declares `hardware.device_tree` and its peripherals, and
/// its firmware loads the resulting tree during bring-up. A record that states
/// neither does not generate one, and that is a refusal rather than a fallback.
pub fn generates(platform: &Platform) -> bool
{
    match platform.hardware
                  .as_ref()
                  .and_then(|hardware| hardware.device_tree.as_ref())
    {
        Some(platform::hardware::DeviceTree::Generated) => true,
        // The record has answered the question, and the answer is not
        // `generated`: a board whose firmware supplies the tree, or one that
        // ships a file, does not want this build to author one.
        Some(_) => false,
        None =>
        {
            platform.emulation
                    .as_ref()
                    .is_some_and(|emulation| emulation.generate_device_tree)
        }
    }
}

/// Builds the tree for the currently selected platform.
pub fn generate() -> Result<Vec<u8>>
{
    let platform = platform::current();
    if !generates(platform)
    {
        return Err(crate::error(format!("platform '{}' does not declare a generated device tree, \
                                         so this build must not produce one",
                                        platform.name)));
    }
    generate_for(platform)
}

/// One node this build writes: its `compatible`, its window, and its interrupt.
struct Node
{
    compatible: String,
    base:       u64,
    bytes:      u64,
    interrupt:  Option<u32>,
    /// Whether the interrupt is stated in the machine's own encoded form rather
    /// than as the flat ARM number a board record carries.
    encoded:    bool
}

/// Builds the tree a platform record describes: its RAM window, its CPUs and the
/// peripheral windows it declares.
///
/// A record with an emulation section describes the machine the emulator models,
/// and that machine's device list is the tree — it models a subset of a real
/// board, so building from the board's list would name hardware the machine does
/// not have. A board describes itself, and its declared peripherals are the tree;
/// there is no emulator to ask.
///
/// A record with no declared RAM or no declared devices is refused rather than
/// emitting an empty tree that both consumers would then have to interpret.
pub fn generate_for(platform: &Platform) -> Result<Vec<u8>>
{
    let emulation = platform.emulation.as_ref();
    // A machine names itself; a board is named by the board it is.
    let machine = match emulation
    {
        Some(emulation) => emulation.machine.clone(),
        None => platform.hardware
                        .as_ref()
                        .map_or_else(|| platform.name.clone(), |hardware| hardware.board.clone())
    };
    let cpu = emulation.and_then(|emulation| emulation.cpu.clone())
                      .unwrap_or_else(|| platform.rust_target.clone());

    let mut tree = Builder::default();
    tree.begin_node("");
    tree.property_u32("#address-cells", &[2]);
    tree.property_u32("#size-cells", &[2]);
    tree.property_string("model", &machine);
    tree.property_string("compatible", &machine);

    let memory = platform.memory().map_err(|error| {
                                       crate::error(format!("platform '{}' declares no usable \
                                                             memory section: {error}",
                                                            platform.name))
                                   })?;
    let ram_base = memory.ram_base();
    let ram_bytes = memory.minimum_ram_bytes();
    tree.begin_node(&format!("memory@{ram_base:x}"));
    tree.property_string("device_type", "memory");
    tree.property_u32("reg", &[high(ram_base), low(ram_base), high(ram_bytes), low(ram_bytes)]);
    tree.end_node();

    let harts = platform.harts();
    tree.begin_node("cpus");
    tree.property_u32("#address-cells", &[1]);
    tree.property_u32("#size-cells", &[0]);
    for hart in 0..harts
    {
        tree.begin_node(&format!("cpu@{hart}"));
        tree.property_string("device_type", "cpu");
        tree.property_u32("reg", &[hart as u32]);
        tree.property_string("compatible", &cpu);
        tree.end_node();
    }
    tree.end_node();

    let nodes = match emulation
    {
        Some(emulation) =>
        {
            let mut nodes = Vec::new();
            for device in &emulation.devices
            {
                let Some((base, bytes)) = device.window(&platform.name)?
                else
                {
                    // A device with no window describes a function rather than a
                    // region, and a tree node without a `reg` would name an
                    // address it does not have.
                    continue;
                };
                nodes.push(Node { compatible: device.model.clone(),
                                  base,
                                  bytes,
                                  interrupt: device.interrupt,
                                  encoded: true });
            }
            nodes
        }
        None =>
        {
            platform.declared_devices()?
                    .into_iter()
                    .map(|device| Node { compatible: device.compatible.clone(),
                                         base: device.base,
                                         bytes: device.bytes,
                                         interrupt: device.interrupt,
                                         encoded: false })
                    .collect()
        }
    };
    if nodes.is_empty()
    {
        return Err(crate::error(format!("platform '{}' declares no peripheral windows; a \
                                         generated device tree would describe no hardware",
                                        platform.name)));
    }
    tree.begin_node("soc");
    tree.property_string("compatible", "simple-bus");
    tree.property_u32("#address-cells", &[2]);
    tree.property_u32("#size-cells", &[2]);
    tree.property_u32("ranges", &[]);
    // The page-granular claims this record declares, so a node can also carry
    // what a compiled-in device row carried: the bus a driver matches on, its
    // 16-byte key, and the flags that describe how the range may be used.
    let claims = platform.claims()?;
    for node in nodes
    {
        tree.begin_node(&format!("{}@{:x}", node.compatible, node.base));
        tree.property_string("compatible", &node.compatible);
        tree.property_u32("reg",
                          &[high(node.base),
                            low(node.base),
                            high(node.bytes),
                            low(node.bytes)]);
        if let Some(interrupt) = node.interrupt
        {
            if node.encoded
            {
                // A machine states an interrupt as its own interrupt controller
                // spells it, so the standard property is the right place.
                tree.property_u32("interrupts", &[interrupt]);
            }
            else
            {
                // A board record states a flat ARM interrupt number, and the
                // standard property means "as this node's interrupt parent spells
                // it". This tree has no interrupt controller node, so it has no
                // parent whose encoding could be written there; spelling the number
                // under `interrupts` would be a claim about a binding that is not in
                // the file. It goes in a platform-owned property instead, which is
                // our statement in the form we actually have it.
                tree.property_u32("platform,arm-interrupt", &[interrupt]);
            }
        }
        if let Some(claim) = platform.claim_for(&claims, node.base, node.bytes)
        {
            // Only where the record declares a claim. The key is the part's own
            // name padded to sixteen bytes, which is what a module matches; the
            // flags say the range is device memory and whether it is coherent.
            // `RESIDENT_OWNED` is policy and comes from the module's own
            // interface declaration, never from here - a tree cannot state who
            // owns a device.
            tree.property_string("bus",
                                 core::str::from_utf8(&claim.bus).unwrap_or_default());
            tree.property("key", &claim.key);
            tree.property_u32("flags", &[claim.flags]);
        }
        tree.end_node();
    }
    tree.end_node();

    tree.end_node();
    Ok(tree.finish(0))
}

const fn high(value: u64) -> u32
{
    (value >> 32) as u32
}

const fn low(value: u64) -> u32
{
    value as u32
}

#[derive(Default)]
struct Builder
{
    structure: Vec<u8>,
    strings:   Vec<u8>
}
impl Builder
{
    fn begin_node(&mut self, name: &str)
    {
        self.token(BEGIN_NODE);
        self.structure.extend_from_slice(name.as_bytes());
        self.structure.push(0);
        self.align();
    }

    fn end_node(&mut self)
    {
        self.token(END_NODE);
    }

    fn property_string(&mut self, name: &str, value: &str)
    {
        let mut bytes = value.as_bytes().to_vec();
        bytes.push(0);
        self.property(name, &bytes);
    }

    fn property_u32(&mut self, name: &str, cells: &[u32])
    {
        let mut bytes = Vec::with_capacity(cells.len() * 4);
        for cell in cells
        {
            bytes.extend_from_slice(&cell.to_be_bytes());
        }
        self.property(name, &bytes);
    }

    fn property(&mut self, name: &str, value: &[u8])
    {
        let name_offset = self.intern(name);
        self.token(PROP);
        self.structure.extend_from_slice(&(value.len() as u32).to_be_bytes());
        self.structure.extend_from_slice(&name_offset.to_be_bytes());
        self.structure.extend_from_slice(value);
        self.align();
    }

    /// Property names are shared, so a repeated name costs one offset.
    fn intern(&mut self, name: &str) -> u32
    {
        if let Some(offset) = find_name(&self.strings, name)
        {
            return offset;
        }
        let offset = self.strings.len() as u32;
        self.strings.extend_from_slice(name.as_bytes());
        self.strings.push(0);
        offset
    }

    fn token(&mut self, token: u32)
    {
        self.structure.extend_from_slice(&token.to_be_bytes());
    }

    fn align(&mut self)
    {
        while self.structure.len() % 4 != 0
        {
            self.structure.push(0);
        }
    }

    fn finish(mut self, boot_cpu: u32) -> Vec<u8>
    {
        self.token(END);
        let reservation_bytes = 16; // one terminating (0, 0) pair
        let structure_offset = HEADER_BYTES + reservation_bytes;
        let strings_offset = structure_offset + self.structure.len();
        let total = strings_offset + self.strings.len();
        let mut blob = Vec::with_capacity(total);
        blob.extend_from_slice(&MAGIC.to_be_bytes());
        blob.extend_from_slice(&(total as u32).to_be_bytes());
        blob.extend_from_slice(&(structure_offset as u32).to_be_bytes());
        blob.extend_from_slice(&(strings_offset as u32).to_be_bytes());
        blob.extend_from_slice(&(HEADER_BYTES as u32).to_be_bytes());
        blob.extend_from_slice(&VERSION.to_be_bytes());
        blob.extend_from_slice(&LAST_COMPATIBLE_VERSION.to_be_bytes());
        blob.extend_from_slice(&boot_cpu.to_be_bytes());
        blob.extend_from_slice(&(self.strings.len() as u32).to_be_bytes());
        blob.extend_from_slice(&(self.structure.len() as u32).to_be_bytes());
        blob.extend_from_slice(&[0u8; 16]);
        blob.extend_from_slice(&self.structure);
        blob.extend_from_slice(&self.strings);
        blob
    }
}

fn find_name(strings: &[u8], name: &str) -> Option<u32>
{
    let needle = name.as_bytes();
    let mut start = 0;
    while start < strings.len()
    {
        let end = strings[start..].iter().position(|byte| *byte == 0)? + start;
        if &strings[start..end] == needle
        {
            return Some(start as u32);
        }
        start = end + 1;
    }
    None
}

/// What a node states about its interrupt, in whichever form the tree uses.
///
/// The two forms are not interchangeable: a controller binding encodes a bank and a
/// bit that only the controller's own layout can decode, while a tree this build
/// generated states the flat ARM number the record holds. Keeping them apart is what
/// stops a comparison from reading one as the other.
#[derive(Clone, Copy, Debug)]
pub enum StatedInterrupt
{
    /// An `interrupts` property, as its interrupt parent spells it.
    Encoded
    {
        bank: u32, bit: u32
    },
    /// A flat ARM interrupt number, from a tree this build generated.
    Arm(u32)
}

impl StatedInterrupt
{
    /// The ARM interrupt number, when the form can be read as one.
    ///
    /// Only the controller's bank 2 has a rule this project has calibrated: a node
    /// there raises ARM interrupt `bit + 32`, confirmed by four points — `<2 17>`
    /// is GPIO, ARM 49; `<2 25>` the PL011, ARM 57; `<2 29>` the RNG, ARM 61;
    /// `<2 30>` the SD controller, ARM 62. Banks 0 and 1 state a different
    /// numbering, and guessing at it would put a number in a driver that nothing
    /// had established.
    pub fn arm_number(&self) -> Option<u32>
    {
        match self
        {
            StatedInterrupt::Arm(number) => Some(*number),
            StatedInterrupt::Encoded { bank: 2,
                                       bit } => Some(bit + 32),
            StatedInterrupt::Encoded { .. } => None
        }
    }
}

/// One node of a tree this build did not produce, reduced to the facts the
/// record also states.
pub struct Observed
{
    pub path:        String,
    /// Every string in the node's `compatible`, in the order the tree lists them.
    ///
    /// All of them, not the first: a node lists its most specific identity first
    /// and progressively more generic ones after it, and a consumer is entitled to
    /// match any of them. The board's own firmware names its console
    /// `arm,pl011-axi` and lists `arm,pl011` beside it; a reader that kept only the
    /// first reported the record's `arm,pl011` absent while the node was in front of
    /// it.
    pub compatibles: Vec<String>,
    pub base:        Option<u64>,
    pub bytes:       Option<u64>,
    /// What the node states about its interrupt, in whichever form it states it.
    pub interrupt:   Option<StatedInterrupt>,
    /// The node's `status`, when it states one. Absence means the tree states no
    /// opinion, which convention reads as usable.
    pub status:      Option<String>
}

impl Observed
{
    /// Whether this node answers to a `compatible`, anywhere in its list.
    pub fn answers_to(&self, compatible: &str) -> bool
    {
        self.compatibles.iter().any(|value| value == compatible)
    }

    /// The node's most specific identity, for a diagnostic.
    pub fn identity(&self) -> &str
    {
        self.compatibles.first().map(String::as_str).unwrap_or(&self.path)
    }

    /// Whether one of the nodes inside an address window states an interrupt.
    ///
    /// A record claims a page and states one interrupt for it, while a tree states
    /// interrupts per functional node: the auxiliary block at `0x7F215000` claims
    /// the page and states no interrupt of its own, while the auxiliary UART and
    /// SPI ports that raise the shared line sit at `0x7F215040` and above, inside
    /// the same page. Looking only at the node the record names would call a device
    /// with an interrupt one that has none. The window searched is the one the
    /// record claims, not the smaller extent the node's own `reg` states.
    pub fn interrupt_in_window(&self, all: &[Observed], base: u64, bytes: u64)
                               -> Option<StatedInterrupt>
    {
        self.interrupt.or_else(|| {
                          all.iter()
                             .filter(|node| node.path != self.path)
                             .filter_map(|node| {
                                 let at = bus_to_arm(node.base?);
                                 if at >= base && at < base.checked_add(bytes)?
                                 {
                                     node.interrupt
                                 }
                                 else
                                 {
                                     None
                                 }
                             })
                             .next()
                      })
    }
}

/// Where the record and a machine's own tree disagree about one device.
pub struct Divergence
{
    pub model:  String,
    pub detail: String
}

struct Cursor<'a>
{
    bytes:    &'a [u8],
    position: usize
}
impl<'a> Cursor<'a>
{
    fn u32(&mut self) -> Result<u32>
    {
        let chunk = self.bytes
                        .get(self.position..self.position + 4)
                        .ok_or_else(|| crate::error("device tree ends inside a token"))?;
        self.position += 4;
        Ok(u32::from_be_bytes(chunk.try_into().unwrap()))
    }

    fn skip(&mut self, bytes: usize) -> Result<()>
    {
        if self.position.checked_add(bytes).is_none_or(|end| end > self.bytes.len())
        {
            return Err(crate::error("device tree ends inside a node"));
        }
        self.position += bytes;
        Ok(())
    }

    fn name(&mut self) -> Result<String>
    {
        let rest = self.bytes
                       .get(self.position..)
                       .ok_or_else(|| crate::error("device tree ends inside a node name"))?;
        let length = rest.iter()
                         .position(|byte| *byte == 0)
                         .ok_or_else(|| crate::error("device tree node name is unterminated"))?;
        self.position += length + 1;
        // Names are padded to a four-byte boundary.
        while self.position % 4 != 0
        {
            self.position += 1;
        }
        Ok(String::from_utf8_lossy(&rest[..length]).into_owned())
    }
}

/// Read a device tree this build did not produce.
///
/// This is a reader, not a validator of our own format: it accepts the
/// two-cell and one-cell `reg` encodings a foreign tree may use, and it refuses
/// only what it cannot interpret. It exists so the record can be checked against
/// a machine's own description instead of against a re-reading of a datasheet.
pub fn read(blob: &[u8]) -> Result<Vec<Observed>>
{
    if blob.len() < HEADER_BYTES
    {
        return Err(crate::error("device tree is shorter than its header"));
    }
    let word = |at: usize| u32::from_be_bytes(blob[at..at + 4].try_into().unwrap());
    if word(0) != MAGIC
    {
        return Err(crate::error("device tree has no valid header"));
    }
    let total = word(4) as usize;
    if total > blob.len()
    {
        return Err(crate::error("device tree states a size larger than the file"));
    }
    let structure_at = word(8) as usize;
    let strings_at = word(12) as usize;
    let strings_len = word(32) as usize;
    let strings = blob.get(strings_at..strings_at + strings_len)
                      .ok_or_else(|| crate::error("device tree strings block is out of bounds"))?;
    let mut cursor = Cursor { bytes:    &blob[..total],
                              position: structure_at };
    let property_name = |offset: usize| -> Result<String> {
        let rest =
            strings.get(offset..)
                   .ok_or_else(|| crate::error("device tree property name is out of bounds"))?;
        let length =
            rest.iter()
                .position(|byte| *byte == 0)
                .ok_or_else(|| crate::error("device tree property name is unterminated"))?;
        Ok(String::from_utf8_lossy(&rest[..length]).into_owned())
    };
    let mut path: Vec<String> = Vec::new();
    let mut nodes: Vec<Observed> = Vec::new();
    // Each open node remembers what it inherits, so closing it restores the
    // parent's scope. `current` is the node properties currently land on: a node
    // is created when it opens, not when its `compatible` appears, because a tree
    // is free to spell its properties in any order and one that reads `reg` before
    // `compatible` would otherwise attribute the window to its parent.
    let mut open: Vec<(Option<usize>, usize, usize, bool)> = Vec::new();
    let mut current: Option<usize> = None;
    // A `reg` is spelled with the cell counts its parent declares. They are
    // inherited and scoped, so they follow the node stack; a node that declares
    // none anywhere above it — the board's own firmware declares none — is read by
    // inferring from the property's length instead, which is the only information
    // the file then carries.
    let mut cells_declared = false;
    let mut address_cells = 1usize;
    let mut size_cells = 1usize;
    loop
    {
        let token = cursor.u32()?;
        match token
        {
            BEGIN_NODE =>
            {
                let name = cursor.name()?;
                path.push(name);
                open.push((current, address_cells, size_cells, cells_declared));
                nodes.push(Observed { path:        path.iter()
                                                       .filter(|segment| !segment.is_empty())
                                                       .cloned()
                                                       .collect::<Vec<_>>()
                                                       .join("/"),
                                      compatibles: Vec::new(),
                                      base:        None,
                                      bytes:       None,
                                      interrupt:   None,
                                      status:      None });
                current = Some(nodes.len() - 1);
            }
            END_NODE =>
            {
                path.pop();
                if let Some((parent, addresses, sizes, declared)) = open.pop()
                {
                    current = parent;
                    address_cells = addresses;
                    size_cells = sizes;
                    cells_declared = declared;
                }
            }
            PROP =>
            {
                let length = cursor.u32()? as usize;
                let name = property_name(cursor.u32()? as usize)?;
                let data =
                    cursor.bytes
                          .get(cursor.position..cursor.position + length)
                          .ok_or_else(|| crate::error("device tree property is out of bounds"))?
                          .to_vec();
                cursor.skip(length)?;
                while cursor.position % 4 != 0
                {
                    cursor.position += 1;
                }
                let cells = || -> Vec<u32> {
                    data.chunks(4)
                        .filter(|chunk| chunk.len() == 4)
                        .map(|chunk| u32::from_be_bytes(chunk.try_into().unwrap()))
                        .collect::<Vec<_>>()
                };
                match name.as_str()
                {
                    "compatible" =>
                    {
                        // A string list: every NUL-terminated run is one identity,
                        // most specific first. Trailing padding is not a name.
                        let mut compatibles = Vec::new();
                        let mut start = 0;
                        for (index, byte) in data.iter().enumerate()
                        {
                            if *byte != 0
                            {
                                continue;
                            }
                            if index > start
                            {
                                compatibles.push(String::from_utf8_lossy(&data[start..index])
                                                    .into_owned());
                            }
                            start = index + 1;
                        }
                        if let Some(index) = current
                        {
                            nodes[index].compatibles = compatibles;
                        }
                    }
                    "reg" =>
                    {
                        let numbers = cells();
                        // With no declaration to follow, the length is the
                        // evidence: one address cell and one size cell is eight
                        // bytes, two of each is sixteen, and a bare one-cell
                        // address with no size is four.
                        let (addresses, sizes) = if cells_declared
                        {
                            (address_cells, size_cells)
                        }
                        else
                        {
                            match numbers.len()
                            {
                                0 | 1 => (numbers.len(), 0),
                                2 => (1, 1),
                                3 => (1, 2),
                                _ => (2, 2)
                            }
                        };
                        if let Some(node) = current.map(|index| &mut nodes[index])
                        {
                            if numbers.len() >= addresses + sizes
                            {
                                let base = numbers[..addresses].iter()
                                                               .fold(0u64, |acc, cell| {
                                                                   (acc << 32) | u64::from(*cell)
                                                               });
                                let bytes = numbers[addresses..addresses + sizes]
                                    .iter()
                                    .fold(0u64, |acc, cell| (acc << 32) | u64::from(*cell));
                                node.base = Some(base);
                                node.bytes = Some(bytes);
                            }
                        }
                    }
                    "interrupts" =>
                    {
                        let numbers = cells();
                        if let (Some(node), Some(first)) =
                            (current.map(|index| &mut nodes[index]), numbers.first())
                        {
                            node.interrupt =
                                Some(StatedInterrupt::Encoded { bank: *first,
                                                                bit:  numbers.get(1)
                                                                             .copied()
                                                                             .unwrap_or(0) });
                        }
                    }
                    "platform,arm-interrupt" =>
                    {
                        // This build's own statement, in the form the record holds
                        // it. A machine's tree never carries it, so it cannot be
                        // confused with the encoded form above.
                        if let (Some(node), Some(number)) =
                            (current.map(|index| &mut nodes[index]), cells().first().copied())
                        {
                            node.interrupt = Some(StatedInterrupt::Arm(number));
                        }
                    }
                    "status" =>
                    {
                        // A tree states a node's usability, and `disabled` is how
                        // the firmware says it will not bring the block up. It is
                        // not part of a node's identity but it is part of what the
                        // machine is configured to do, so it is carried rather than
                        // dropped — the board's own tree marks the Arasan block
                        // `disabled` while marking the other SD controller `okay`,
                        // and a comparison that ignored `status` could not show it.
                        let end = data.iter().position(|byte| *byte == 0).unwrap_or(data.len());
                        if let Some(node) = current.map(|index| &mut nodes[index])
                        {
                            node.status = Some(String::from_utf8_lossy(&data[..end]).into_owned());
                        }
                    }
                    "#address-cells" =>
                    {
                        address_cells = cells().first().copied().unwrap_or(1) as usize;
                        cells_declared = true;
                    }
                    "#size-cells" =>
                    {
                        size_cells = cells().first().copied().unwrap_or(1) as usize;
                        cells_declared = true;
                    }
                    _ =>
                    {}
                }
            }
            END =>
            {
                break;
            }
            other =>
            {
                return Err(crate::error(format!("device tree has an unknown token {other:#x}")));
            }
        }
    }
    Ok(nodes)
}

/// Translate a VideoCore bus address into the ARM view of the same registers.
///
/// The peripheral aperture is at `0x7E000000` for the VideoCore and `0x3F000000`
/// for the ARM cores, with the same offset inside it, so one translation serves
/// every peripheral. Addresses outside the aperture — a `reg` that already names
/// memory — are returned unchanged.
pub fn bus_to_arm(address: u64) -> u64
{
    const BUS_BASE: u64 = 0x7e00_0000;
    const ARM_BASE: u64 = 0x3f00_0000;
    const APERTURE: u64 = 0x0200_0000;
    if address >= BUS_BASE && address - BUS_BASE < APERTURE
    {
        ARM_BASE + (address - BUS_BASE)
    }
    else
    {
        address
    }
}

/// The outcome of checking the record against a machine's own tree.
pub struct CrossCheck
{
    /// Findings that mean the record or this platform is wrong, or that nobody has
    /// explained yet.
    pub warnings:                 Vec<Divergence>,
    /// Findings that are expected, each carrying the record's own statement of why.
    /// They are still printed — a check that silently accepted differences would
    /// not be a check — but they are not warnings, because an expected difference
    /// is not something anyone can act on.
    pub expected:                 Vec<Divergence>,
    /// Devices for which the record declares no interrupt and the tree states none
    /// either. That is agreement, so it is counted rather than reported, but it is
    /// counted out loud: an interrupt comparison that quietly skipped these would be
    /// indistinguishable from one that never ran.
    pub agreed_without_interrupt: Vec<String>
}

/// Compare every peripheral the record declares against a machine's own tree.
///
/// This validates the *record*, not the machine: the tree is what is actually in
/// front of the kernel, and the record is a declaration used for module selection.
/// A divergence is reported with both values so it can be resolved rather than
/// silently preferred.
///
/// **The comparison is pre-overlay.** A board's published DTB is the base tree;
/// `start.elf` applies the `config.txt` overlays to a copy in memory and hands the
/// result to whatever it starts. The file on the card is therefore the machine
/// before its configuration, and the tree the kernel walks is the machine after it.
/// Comparing the file is still worth doing, because addresses and interrupts do not
/// change under an overlay — only `status` does — but a `status` difference here may
/// simply be an overlay this build does not apply, and is treated accordingly.
///
/// Interrupt **presence** is compared, not interrupt numbers. The record states a
/// flat number from the SoC's peripheral interrupt list, and a tree states an
/// `armctrl` `<bank bit>` pair, so the two are not the same quantity and a
/// translation between them would have to be derived from the interrupt
/// controller's own layout rather than from the pair of sources being checked —
/// deriving it from those would make the check agree with itself. Numbers are
/// therefore left to be read side by side and resolved by later work; what this
/// reports is a record that claims an interrupt the machine's tree does not
/// corroborate at all.
pub fn compare(platform: &Platform, blob: &[u8]) -> Result<CrossCheck>
{
    let observed = read(blob)?;
    let mut check = CrossCheck { warnings:                 Vec::new(),
                                 expected:                 Vec::new(),
                                 agreed_without_interrupt: Vec::new() };
    let mut without_interrupt: Vec<String> = Vec::new();
    for device in platform.declared_devices()?
    {
        // A node answers to any string in its `compatible` list; a consumer that
        // matched only the first would miss `arm,pl011` on a node listing
        // `arm,pl011-axi` ahead of it.
        let node = observed.iter().find(|node| node.answers_to(&device.compatible));
        let Some(node) = node
        else
        {
            check.warnings.push(Divergence { model:  device.model.clone(),
                                             detail: format!("the machine's tree has no node \
                                                              compatible with '{}', so the \
                                                              record declares a device this \
                                                              machine does not describe",
                                                             device.compatible) });
            continue;
        };
        // The board's tree states peripherals in the VideoCore's bus view, and the
        // record in the ARM view of the same registers. Comparing them without
        // translating would report every device as a divergence — which is the same
        // class of mistake as comparing the wrong vocabulary, and it is what the
        // first corrected run did.
        let base = node.base.map(bus_to_arm);
        if base != Some(device.base)
        {
            check.warnings.push(Divergence { model:  device.model.clone(),
                                             detail: format!("record says {:#x}, the tree says {}",
                                                             device.base,
                                                             match (node.base, base)
                                                             {
                                                                 (Some(raw), Some(arm)) =>
                                                                 {
                                                                     format!("{raw:#x} in the bus \
                                                                              view, {arm:#x} in \
                                                                              the ARM view")
                                                                 }
                                                                 _ => "no reg property".into()
                                                             }) });
        }
        // A size difference is only interesting in one direction. The record states
        // the page a driver claims and the tree states the register block inside it,
        // so the record is normally the larger of the two; a tree that is larger
        // means the record under-declares the device, which is a real defect.
        if let Some(bytes) = node.bytes
        {
            if bytes > device.bytes
            {
                check.warnings.push(Divergence { model:  device.model.clone(),
                                                 detail: format!("the record declares {:#x} \
                                                                  bytes but the tree gives \
                                                                  {bytes:#x}",
                                                                 device.bytes) });
            }
        }
        // `status` is the machine's configuration of software it would itself run,
        // not a statement about the hardware: a peripheral the firmware does not
        // enable is one no Linux driver would probe, while its registers sit at the
        // address the record gives and a bare-metal driver configures it directly.
        // A difference here is therefore always an accounted-for finding rather than
        // a warning — and the record has to supply the reason it appears beside, so
        // this cannot become a place where differences go to be forgotten.
        if let Some(status) = &node.status
        {
            if status != "okay" && status != "ok"
            {
                let driven = match device.disposition
                {
                    Disposition::Active => "the platform drives this block itself",
                    Disposition::Declared =>
                    {
                        "nothing drives this block, so the firmware's configuration of it is its \
                         own choice"
                    }
                };
                let detail = format!("the machine's tree marks it status = '{status}'; {driven}");
                let detail = match &device.note
                {
                    Some(note) => format!("{detail} ({note})"),
                    None => detail
                };
                check.expected.push(Divergence { model: device.model.clone(),
                                                 detail })
            }
        }
        // The interrupt may be stated on a node the window contains rather than on
        // the node itself: the auxiliary block is a mux whose three children share
        // one line, and the record, which claims the block, states the line once.
        let interrupt = node.interrupt_in_window(&observed, device.base, device.bytes);
        match (interrupt, device.interrupt)
        {
            (None, Some(number)) =>
            {
                check.warnings.push(Divergence { model:  device.model.clone(),
                                                 detail: format!("the record declares interrupt \
                                                                  {number} and neither this node \
                                                                  nor any node in the window \
                                                                  states one") });
            }
            (None, None) => without_interrupt.push(device.model.clone()),
            (Some(stated), Some(number)) =>
            {
                // Reported only where the form can be read as an ARM number. Where
                // it cannot — the controller's banks 0 and 1 state a different
                // numbering — the pair is left for a later reader instead of being
                // decoded by guesswork.
                if let Some(arm) = stated.arm_number()
                {
                    if arm != number
                    {
                        check.warnings
                             .push(Divergence { model:  device.model.clone(),
                                                detail: format!("the record declares interrupt \
                                                                 {number}, and the tree states one \
                                                                 that reads as {arm}") });
                    }
                }
            }
            (Some(_), None) =>
            {} /* Neither the record's claim to make nor ours to check. */
        }
    }
    // A node the machine's configuration disables is one thing; two nodes at the
    // same address, only one of them enabled, is another — the machine is saying
    // that address belongs to a different function. That is what the board's own
    // tree says about the Arasan SD controller: `mmc@7e300000` is `disabled` and
    // `mmcnr@7e300000`, the same registers under the same compatible, is `okay`
    // with a wireless child. Reported separately because it cannot be resolved by
    // reading either source harder.
    for device in platform.declared_devices()?
    {
        let Some(node) = observed.iter().find(|node| node.answers_to(&device.compatible))
        else
        {
            continue;
        };
        if node.status.as_deref().is_none_or(|status| status == "okay" || status == "ok")
        {
            continue;
        }
        let also_at_this_address: Vec<&Observed> =
            observed.iter()
                    .filter(|other| {
                        other.path != node.path
                        && other.base == node.base
                        && other.status.as_deref().is_some_and(|status| status != "disabled")
                    })
                    .collect();
        for other in also_at_this_address
        {
            let detail = format!("the tree disables this node and enables {} at the same address \
                                  ({}), so the machine's own description assigns these registers \
                                  to a different function",
                                 other.path,
                                 other.compatibles.join(", "));
            let detail = match &device.note
            {
                Some(note) => format!("{detail} ({note})"),
                None => detail
            };
            check.warnings.push(Divergence { model: device.model.clone(),
                                             detail });
        }
    }
    // Devices for which the record declares no interrupt are counted rather than
    // reported as a finding: that is the record and the tree agreeing, not a
    // difference. The count is surfaced so the agreement is visible instead of
    // looking like the comparison never looked at interrupts at all.
    check.agreed_without_interrupt = without_interrupt;
    Ok(check)
}


