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

/// Checks that a platform takes a generated tree, returning a named error when
/// it does not. Split out from `generate` so both branches are testable without
/// depending on which platform the environment selects.
pub fn require_generated(platform: &Platform) -> Result<()>
{
    let declared = platform.hardware.as_ref().and_then(|hardware| hardware.device_tree.as_ref());
    match declared
    {
        Some(platform::hardware::DeviceTree::Generated) => Ok(()),
        Some(other) =>
        {
            Err(crate::error(format!("platform '{}' declares device_tree {other:?}, so it does \
                                      not take a generated tree",
                                     platform.name)))
        }
        None =>
        {
            Err(crate::error(format!("platform '{}' declares no device_tree; a generated tree \
                                      must be declared rather than assumed",
                                     platform.name)))
        }
    }
}

/// Builds the tree for the currently selected platform, refusing a platform that
/// does not declare a generated tree. Without this check the record's
/// declaration would be decorative and a board whose firmware supplies its own
/// tree would silently receive a generated one instead.
pub fn generate() -> Result<Vec<u8>>
{
    let platform = platform::current();
    require_generated(platform)?;
    generate_for(platform)
}

/// Builds the tree a platform record describes: its RAM window, its CPUs and the
/// peripheral windows it declares. A platform with no declared RAM or no
/// declared devices is refused rather than emitting an empty tree that both
/// consumers would then have to interpret.
pub fn generate_for(platform: &Platform) -> Result<Vec<u8>>
{
    let mut tree = Builder::default();
    tree.begin_node("");
    tree.property_u32("#address-cells", &[2]);
    tree.property_u32("#size-cells", &[2]);
    tree.property_string("model", &platform.name);
    tree.property_string("compatible", &platform.name);

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
        tree.property_string("compatible", &platform.rust_target);
        tree.end_node();
    }
    tree.end_node();

    let devices = platform.declared_devices()?;
    if devices.is_empty()
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
    for device in devices
    {
        tree.begin_node(&format!("{}@{:x}", device.compatible, device.base));
        tree.property_string("compatible", &device.compatible);
        tree.property_u32("reg",
                          &[high(device.base),
                            low(device.base),
                            high(device.bytes),
                            low(device.bytes)]);
        // The record states a flat ARM interrupt number, and the standard
        // `interrupts` property means "as this node's interrupt parent spells it".
        // This tree has no interrupt controller node, so it has no parent whose
        // encoding could be written there — spelling the number under `interrupts`
        // would be a claim about a binding that is not in the file. The number goes
        // in a platform-owned property instead: it is our statement, in the form we
        // actually have it, and a machine's own tree will carry the encoded form.
        if let Some(interrupt) = device.interrupt
        {
            tree.property_u32("platform,arm-interrupt", &[interrupt]);
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

#[cfg(test)]
mod cross_check_tests
{
    use super::{HEADER_BYTES, MAGIC, compare, generate_for, read};

    /// The reader and the comparator, exercised on real bytes: a tree this build
    /// produced is walked back with the foreign-tree reader and must agree with the
    /// record that produced it — every window, every interrupt, every compatible.
    ///
    /// This is what makes the comparison trustworthy in the other direction. If our
    /// own tree and the record it came from cannot agree, a difference found against
    /// a real machine's tree means nothing.
    #[test]
    fn the_record_and_its_own_generated_tree_agree_on_everything()
    {
        // A record that declares no board section describes no peripherals, so
        // the check needs the hardware section this comparison exists for.
        let platform = super::super::platform::load("raspi3b").expect("record loads");
        let devices = platform.declared_devices().expect("record declares devices");
        assert!(!devices.is_empty(), "the record declares no devices to compare");
        assert!(devices.iter().any(|device| device.interrupt.is_some()),
                "the record declares no interrupts, so the interrupt half is untested");
        let blob = generate_for(&platform).expect("tree generates");
        let nodes = read(&blob).expect("the foreign-tree reader parses our own tree");
        assert!(!nodes.is_empty(), "the reader found no nodes");
        for device in &devices
        {
            let node = nodes.iter()
                            .find(|node| node.answers_to(&device.compatible))
                            .unwrap_or_else(|| panic!("no node for {}", device.describe()));
            assert_eq!(node.base, Some(device.base), "{}", device.describe());
            assert_eq!(node.interrupt.and_then(|stated| stated.arm_number()),
                       device.interrupt,
                       "{}",
                       device.describe());
        }
        let check = compare(&platform, &blob).expect("comparison runs");
        let report = |findings: &[crate::devicetree::Divergence]| {
            findings.iter()
                    .map(|d| format!("{}: {}", d.model, d.detail))
                    .collect::<Vec<_>>()
                    .join("; ")
        };
        assert!(check.warnings.is_empty(), "{}", report(&check.warnings));
        assert!(check.expected.is_empty(), "{}", report(&check.expected));
    }

    /// A node's `compatible` is a list, most specific first, and a consumer may
    /// match any of its strings. Reading only the first is what made this check
    /// report the PL011 absent while the node sat in the tree in front of it: the
    /// board's firmware names its console `arm,pl011-axi` and lists `arm,pl011`
    /// beside it.
    #[test]
    fn every_compatible_string_is_read_not_just_the_first()
    {
        // A tree with one node whose compatible is a two-string list, built here so
        // the test states the property directly rather than depending on whatever
        // some other generator happens to emit.
        const BEGIN_NODE: u32 = 1;
        const END_NODE: u32 = 2;
        const PROP: u32 = 3;
        const END: u32 = 9;
        let strings = b"compatible\0";
        let mut structure = Vec::new();
        let mut node_name = |structure: &mut Vec<u8>, name: &str| {
            structure.extend_from_slice(&BEGIN_NODE.to_be_bytes());
            structure.extend_from_slice(name.as_bytes());
            structure.push(0);
            while structure.len() % 4 != 0
            {
                structure.push(0);
            }
        };
        node_name(&mut structure, "");
        node_name(&mut structure, "soc");
        let compatible = b"arm,pl011-axi\0arm,pl011\0";
        structure.extend_from_slice(&PROP.to_be_bytes());
        structure.extend_from_slice(&(compatible.len() as u32).to_be_bytes());
        structure.extend_from_slice(&0u32.to_be_bytes());
        structure.extend_from_slice(compatible);
        while structure.len() % 4 != 0
        {
            structure.push(0);
        }
        structure.extend_from_slice(&END_NODE.to_be_bytes());
        structure.extend_from_slice(&END_NODE.to_be_bytes());
        structure.extend_from_slice(&END.to_be_bytes());
        let structure_at = HEADER_BYTES as u32;
        let strings_at = structure_at + structure.len() as u32;
        let mut blob = Vec::new();
        blob.extend_from_slice(&MAGIC.to_be_bytes());
        blob.extend_from_slice(&(strings_at + strings.len() as u32).to_be_bytes());
        blob.extend_from_slice(&structure_at.to_be_bytes());
        blob.extend_from_slice(&strings_at.to_be_bytes());
        blob.extend_from_slice(&0u32.to_be_bytes()); /* no memory reservations */
        blob.extend_from_slice(&17u32.to_be_bytes()); /* version */
        blob.extend_from_slice(&16u32.to_be_bytes()); /* last compatible version */
        blob.extend_from_slice(&0u32.to_be_bytes()); /* boot cpu */
        blob.extend_from_slice(&(strings.len() as u32).to_be_bytes());
        blob.extend_from_slice(&(structure_at + structure.len() as u32
                                 - structure_at).to_be_bytes());
        while blob.len() < HEADER_BYTES
        {
            blob.push(0);
        }
        blob.extend_from_slice(&structure);
        blob.extend_from_slice(strings);
        let nodes = read(&blob).expect("the synthetic tree parses");
        let soc = nodes.iter().find(|node| node.path == "soc").expect("the node is found");
        assert_eq!(soc.compatibles, vec!["arm,pl011-axi".to_string(), "arm,pl011".to_string()]);
        assert!(soc.answers_to("arm,pl011"),
                "the second string must be matchable: a consumer asking for the generic identity \
                 is entitled to find this node");
        assert!(!soc.answers_to("arm,primecell"), "a string the node does not list");
    }
}

#[cfg(test)]
mod tests
{
    use super::*;

    /// Reads the emitted blob back so the test asserts the structure a consumer
    /// would see, not that the generator ran. This is the only artifact both the
    /// bootloader and the kernel depend on, so it is parsed rather than trusted.
    struct Reader<'a>
    {
        structure: &'a [u8],
        strings:   &'a [u8],
        cursor:    usize
    }
    impl<'a> Reader<'a>
    {
        fn new(blob: &'a [u8]) -> Self
        {
            assert_eq!(u32::from_be_bytes(blob[0..4].try_into().unwrap()), MAGIC);
            let total = u32::from_be_bytes(blob[4..8].try_into().unwrap()) as usize;
            assert_eq!(total, blob.len(), "header total size must match the blob");
            let structure = u32::from_be_bytes(blob[8..12].try_into().unwrap()) as usize;
            let strings = u32::from_be_bytes(blob[12..16].try_into().unwrap()) as usize;
            let structure_bytes = u32::from_be_bytes(blob[36..40].try_into().unwrap()) as usize;
            Self { structure: &blob[structure..structure + structure_bytes],
                   strings:   &blob[strings..],
                   cursor:    0 }
        }

        fn u32(&mut self) -> u32
        {
            let value = u32::from_be_bytes(self.structure[self.cursor..self.cursor + 4].try_into()
                                                                                       .unwrap());
            self.cursor += 4;
            value
        }

        fn name(&mut self) -> String
        {
            let start = self.cursor;
            while self.structure[self.cursor] != 0
            {
                self.cursor += 1;
            }
            let name = String::from_utf8(self.structure[start..self.cursor].to_vec()).unwrap();
            self.cursor += 1;
            while self.cursor % 4 != 0
            {
                self.cursor += 1;
            }
            name
        }

        fn skip(&mut self, bytes: usize)
        {
            self.cursor += bytes;
            while self.cursor % 4 != 0
            {
                self.cursor += 1;
            }
        }

        fn string_at(&self, offset: usize) -> String
        {
            let end = self.strings[offset..].iter().position(|byte| *byte == 0).unwrap() + offset;
            String::from_utf8(self.strings[offset..end].to_vec()).unwrap()
        }

        /// Collects `(node path, compatible, reg cells)` for every node, so a
        /// test can ask what a consumer would find rather than checking offsets.
        fn nodes(&mut self) -> Vec<(String, Option<String>, Vec<u32>)>
        {
            // Token values are literals from the Devicetree Specification rather
            // than the emitter's constants, so an emitter that wrote the wrong
            // token would not be confirmed by a parser repeating the mistake.
            const FDT_BEGIN_NODE: u32 = 1;
            const FDT_END_NODE: u32 = 2;
            const FDT_PROP: u32 = 3;
            const FDT_END: u32 = 9;
            let mut stack: Vec<String> = Vec::new();
            let mut found = Vec::new();
            loop
            {
                let token = self.u32();
                match token
                {
                    FDT_BEGIN_NODE =>
                    {
                        let name = self.name();
                        stack.push(name);
                    }
                    FDT_END_NODE =>
                    {
                        stack.pop();
                    }
                    FDT_PROP =>
                    {
                        let length = self.u32() as usize;
                        let name_offset = self.u32() as usize;
                        let name = self.string_at(name_offset);
                        let start = self.cursor;
                        let mut cells = Vec::new();
                        if length % 4 == 0
                        {
                            for chunk in self.structure[start..start + length].chunks(4)
                            {
                                cells.push(u32::from_be_bytes(chunk.try_into().unwrap()));
                            }
                        }
                        // The root node is unnamed, so joining the stack
                        // directly would produce a leading separator.
                        let path = stack.iter()
                                        .filter(|segment| !segment.is_empty())
                                        .cloned()
                                        .collect::<Vec<_>>()
                                        .join("/");
                        if name == "compatible" || name == "reg"
                        {
                            // A compatible value is an inline string in the
                            // property data, not an offset into the strings
                            // block, which only holds property names.
                            let text = if name == "compatible"
                            {
                                let end =
                                    self.structure[start..start + length].iter()
                                                                         .position(|byte| {
                                                                             *byte == 0
                                                                         })
                                                                         .unwrap_or(length);
                                Some(String::from_utf8(self.structure[start..start + end]
                                                           .to_vec())
                                     .unwrap())
                            }
                            else
                            {
                                None
                            };
                            let entry = found.iter_mut().find(|(existing, _, _)| *existing == path);
                            match entry
                            {
                                Some((_, compatible, reg)) =>
                                {
                                    if name == "compatible"
                                    {
                                        *compatible = text;
                                    }
                                    else
                                    {
                                        *reg = cells;
                                    }
                                }
                                None =>
                                {
                                    found.push((path,
                                                if name == "compatible" { text } else { None },
                                                if name == "reg" { cells } else { Vec::new() }));
                                }
                            }
                        }
                        self.skip(length);
                    }
                    FDT_END => break,
                    other => panic!("unexpected token {other}")
                }
            }
            found
        }
    }

    /// Loads the arm64 record explicitly rather than reading the ambient
    /// platform selection, so the suite gives the same answer on any checkout.
    fn generated() -> (Vec<u8>, Platform)
    {
        let platform = platform::load("raspi3b").expect("the arm64 record must load");
        let blob = generate_for(&platform).expect("generated tree");
        (blob, platform)
    }

    #[test]
    fn a_platform_that_does_not_declare_a_generated_tree_is_refused()
    {
        let generated = platform::load("raspi3b").expect("arm64 record");
        assert!(require_generated(&generated).is_ok(),
                "the arm64 record declares a generated tree");
        let firmware = platform::load("qemu_riscv").expect("riscv record");
        let error = require_generated(&firmware).expect_err("a firmware platform must be refused");
        let message = error.to_string();
        assert!(message.contains("qemu_riscv") && message.contains("device_tree"),
                "the refusal must name the platform and the missing declaration: {message}");
    }

    #[test]
    fn generated_tree_declares_memory_cpus_and_every_declared_device_window()
    {
        let (blob, platform) = generated();
        let mut reader = Reader::new(&blob);
        let nodes = reader.nodes();
        let memory = platform.memory().expect("memory section");
        let memory_node = nodes.iter()
                               .find(|(path, _, _)| path.starts_with("memory@"))
                               .expect("the tree must describe RAM");
        assert_eq!(memory_node.2,
                   vec![high(memory.ram_base()),
                        low(memory.ram_base()),
                        high(memory.minimum_ram_bytes()),
                        low(memory.minimum_ram_bytes())],
                   "the memory node must carry the record's RAM base and size");
        let cpus = nodes.iter().filter(|(path, _, _)| path.starts_with("cpus/cpu@")).count();
        assert_eq!(cpus, platform.harts(), "one cpu node per declared hart");
        for device in platform.declared_devices().expect("declared devices")
        {
            let path = format!("soc/{}@{:x}", device.compatible, device.base);
            let node = nodes.iter()
                            .find(|(candidate, _, _)| *candidate == path)
                            .unwrap_or_else(|| panic!("the tree must describe {path}"));
            assert_eq!(node.1.as_deref(), Some(device.compatible.as_str()));
            assert_eq!(node.2,
                       vec![high(device.base),
                            low(device.base),
                            high(device.bytes),
                            low(device.bytes)],
                       "{} must carry its declared window",
                       device.compatible);
        }
    }

    #[test]
    fn every_declared_device_kind_is_findable_by_compatible()
    {
        let (blob, _) = generated();
        let mut reader = Reader::new(&blob);
        let nodes = reader.nodes();
        let compatible: Vec<&str> =
            nodes.iter().filter_map(|(_, value, _)| value.as_deref()).collect();
        // A consumer finds a device by the tree's `compatible`, so what must be
        // present is the string the record declares for each device — not this
        // platform's inventory name for the part, which is a different vocabulary
        // and is what a first version of this check got wrong.
        for expected in ["arm,pl011", "brcm,bcm2835-mmc"]
        {
            assert!(compatible.contains(&expected),
                    "a consumer looking up {expected:?} must find it; tree has {compatible:?}");
        }
        // And the inventory name must *not* be what a consumer has to know.
        for internal in ["pl011", "sdhci", "bcm2835-mailbox"]
        {
            assert!(!compatible.contains(&internal),
                    "{internal:?} is this platform's name for the part, not the tree's; a \
                     consumer must not have to know it");
        }
    }

    #[test]
    fn the_header_is_well_formed_and_every_offset_is_in_bounds()
    {
        let (blob, _) = generated();
        let word = |offset: usize| u32::from_be_bytes(blob[offset..offset + 4].try_into().unwrap());
        assert_eq!(word(0), MAGIC, "a consumer rejects a tree without the magic");
        assert_eq!(word(4) as usize, blob.len(), "totalsize must cover the whole blob");
        assert_eq!(word(20), VERSION, "version must be one consumers accept");
        assert!(word(24) <= VERSION, "last compatible version cannot exceed version");
        let structure = word(8) as usize;
        let strings = word(12) as usize;
        let reservations = word(16) as usize;
        let structure_bytes = word(36) as usize;
        let strings_bytes = word(32) as usize;
        // A corrupt offset is the failure that presents as "no devices found"
        // rather than as an error, so every one is checked against the blob.
        assert!(reservations >= HEADER_BYTES && reservations + 16 <= blob.len(),
                "memory reservation block must lie inside the blob");
        assert!(structure >= HEADER_BYTES && structure + structure_bytes <= blob.len(),
                "structure block must lie inside the blob");
        assert!(strings >= HEADER_BYTES && strings + strings_bytes <= blob.len(),
                "strings block must lie inside the blob");
        assert_eq!(structure % 4, 0, "the structure block must be word aligned");
        assert_eq!(strings % 4, 0, "the strings block must be word aligned");
        assert!(structure_bytes > 0 && strings_bytes > 0, "both blocks must be non-empty");
    }

    #[test]
    fn the_blob_is_reproducible()
    {
        let (first, _) = generated();
        let (second, _) = generated();
        assert_eq!(first, second, "the emitted tree must not depend on build ordering");
    }
}
