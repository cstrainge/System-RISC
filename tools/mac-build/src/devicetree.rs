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
            platform::{self, Platform}};

const MAGIC: u32 = 0xd00d_feed;
const VERSION: u32 = 17;
const LAST_COMPATIBLE_VERSION: u32 = 16;
const HEADER_BYTES: usize = 40;

const BEGIN_NODE: u32 = 1;
const END_NODE: u32 = 2;
const PROP: u32 = 3;
const END: u32 = 9;

/// Checks that a platform takes a generated tree, returning a named error when
/// it does not. Split out from `generate` so both branches are testable without
/// depending on which platform the environment selects.
pub fn require_generated(platform: &Platform) -> Result<()>
{
    let declared = platform.hardware
                           .as_ref()
                           .and_then(|hardware| hardware.device_tree.as_ref());
    match declared
    {
        Some(platform::hardware::DeviceTree::Generated) => Ok(()),
        Some(other) =>
        {
            Err(crate::error(format!("platform '{}' declares device_tree {other:?}, so it does not \
                                      take a generated tree",
                                     platform.name)))
        }
        None =>
        {
            Err(crate::error(format!("platform '{}' declares no device_tree; a generated tree must \
                                      be declared rather than assumed",
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
                                            crate::error(format!("platform '{}' declares no \
                                                                  usable memory section: {error}",
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
                          &[high(device.base), low(device.base), high(device.bytes),
                            low(device.bytes)]);
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
                   strings: &blob[strings..],
                   cursor: 0 }
        }

        fn u32(&mut self) -> u32
        {
            let value = u32::from_be_bytes(self.structure[self.cursor..self.cursor + 4]
                                               .try_into()
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
            let mut stack: Vec<String> = Vec::new();
            let mut found = Vec::new();
            loop
            {
                let token = self.u32();
                match token
                {
                    BEGIN_NODE =>
                    {
                        let name = self.name();
                        stack.push(name);
                    }
                    END_NODE =>
                    {
                        stack.pop();
                    }
                    PROP =>
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
                                let end = self.structure[start..start + length]
                                          .iter()
                                          .position(|byte| *byte == 0)
                                          .unwrap_or(length);
                                Some(String::from_utf8(self.structure[start..start + end]
                                                           .to_vec())
                                     .unwrap())
                            }
                            else
                            {
                                None
                            };
                            let entry = found.iter_mut()
                                             .find(|(existing, _, _)| *existing == path);
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
                    END => break,
                    other => panic!("unexpected token {other}"),
                }
            }
            found
        }
    }

    /// Loads the arm64 record explicitly rather than reading the ambient
    /// platform selection, so the suite gives the same answer on any checkout.
    fn generated() -> (Vec<u8>, Platform)
    {
        let platform = platform::load("qemu_arm64").expect("the arm64 record must load");
        let blob = generate_for(&platform).expect("generated tree");
        (blob, platform)
    }

    #[test]
    fn a_platform_that_does_not_declare_a_generated_tree_is_refused()
    {
        let generated = platform::load("qemu_arm64").expect("arm64 record");
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
        for expected in ["pl011", "sdhci"]
        {
            assert!(compatible.contains(&expected),
                    "a consumer looking up {expected:?} must find it; tree has {compatible:?}");
        }
    }

    #[test]
    fn the_header_is_well_formed_and_every_offset_is_in_bounds()
    {
        let (blob, _) = generated();
        let word = |offset: usize| {
            u32::from_be_bytes(blob[offset..offset + 4].try_into().unwrap())
        };
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
