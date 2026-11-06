//! Data-only admission for the selected QEMU legacy CLINT binding. No MMIO or
//! hart release occurs here. Numeric hart IDs never determine register offsets.
//!
//! This is a **consumer** of the shared reader, not part of it. What it derives -
//! a CLINT `msip` cell per hart - is RISC-V mechanism, and a reader that knew
//! what a CLINT is would put board mechanism where every architecture has to look
//! at it. What it needs from the reader is node identity, and that is all it
//! takes.
#![forbid(unsafe_code)]
use alloc::vec::Vec;
use core::ops::Range;

use mac_abi::boot::topology::{Cpu, Topology};
use mac_devicetree::{Node, Tree};

use super::{Error, Result};

pub struct MonitorRoute
{
    pub hart:   u64,
    /// On this platform an inter-processor signal is a write to a CLINT cell, so
    /// the route is that cell's address. It is published as the entry's
    /// platform-encoded signalling target, which is what the ABI carries.
    pub signal: u64
}
pub struct MonitorRoutes
{
    pub timebase_hz: u64,
    pub routes:      Vec<MonitorRoute>
}

fn push<T>(values: &mut Vec<T>, value: T) -> Result<()>
{
    values.try_reserve(1)
          .map_err(|_| Error::AllocationFailed("monitor DT metadata allocation failed"))?;
    values.push(value);
    Ok(())
}
fn once<T>(slot: &mut Option<T>, value: T) -> Result<()>
{
    if slot.is_some()
    {
        return Err(Error::Malformed("monitor DT repeats a binding property"));
    }
    *slot = Some(value);
    Ok(())
}
fn word(bytes: &[u8], offset: usize) -> Result<u32>
{
    let end = offset.checked_add(4).ok_or(Error::Malformed("monitor offset overflow"))?;
    let slice = bytes.get(offset..end).ok_or(Error::Malformed("truncated monitor property"))?;
    Ok(u32::from_be_bytes(slice.try_into().unwrap()))
}
fn cells(bytes: &[u8], count: u32) -> Result<u64>
{
    match count
    {
        1 => Ok(u64::from(word(bytes, 0)?)),
        2 => Ok((u64::from(word(bytes, 0)?) << 32) | u64::from(word(bytes, 4)?)),
        _ => Err(Error::Malformed("monitor addresses require one or two cells"))
    }
}
/// Whether a `status` property leaves a node enabled.
///
/// A status this binding does not know is refused rather than read as enabled or
/// as absent: either guess would silently include or exclude a CPU.
fn enabled_of(status: Option<&str>) -> Result<bool>
{
    match status
    {
        None | Some("okay") | Some("ok") => Ok(true),
        Some("disabled") | Some("reserved") | Some("fail") => Ok(false),
        Some(other) if other.starts_with("fail-") => Ok(false),
        Some(_) => Err(Error::Malformed("monitor status is not a recognized single string"))
    }
}
fn contains_compatible(value: &[u8], name: &[u8]) -> Result<bool>
{
    if value.last() != Some(&0)
    {
        return Err(Error::Malformed("monitor compatible is not a string list"));
    }
    let mut found = false;
    for item in value[..value.len() - 1].split(|byte| *byte == 0)
    {
        if item.is_empty()
        {
            return Err(Error::Malformed("monitor compatible contains an empty string"));
        }
        found |= item == name;
    }
    Ok(found)
}
fn legacy_clint(value: &[u8]) -> bool
{
    matches!(value, b"sifive,clint0\0riscv,clint0\0" | b"sifive,clint0\0" | b"riscv,clint0\0")
}

/// A bus's declared cell geometry, which must be explicit.
fn geometry(address: Option<u32>, size: Option<u32>) -> Result<(u32, u32)>
{
    match (address, size)
    {
        (Some(a @ (1 | 2)), Some(s @ (1 | 2))) => Ok((a, s)),
        _ =>
        {
            Err(Error::Malformed("monitor bus requires explicit one-or-two address and size \
                                  cells"))
        }
    }
}

/// One node, copied out of the walk so the interpretation below can look at all
/// of them at once. Children arrive before their parents, so nothing can be
/// decided about a container while the walk is still running.
struct Record<'a>
{
    index:                usize,
    parent_index:         Option<usize>,
    parent:               Option<&'a str>,
    name:                 &'a str,
    depth:                usize,
    own_address_cells:    Option<u32>,
    own_size_cells:       Option<u32>,
    ranges:               Option<&'a [u8]>,
    compatible_list:      Option<&'a [u8]>,
    device_type:          Option<&'a str>,
    reg:                  Option<&'a [u8]>,
    interrupts_extended:  Option<&'a [u8]>,
    status:               Option<&'a str>,
    interrupt_controller: bool,
    interrupt_cells:      Option<u32>,
    phandle:              Option<u32>,
    linux_phandle:        Option<u32>,
    timebase_frequency:   Option<u32>
}
impl<'a> Record<'a>
{
    fn from(node: &Node<'a>) -> Self
    {
        Self { index:                node.index,
               parent_index:         node.parent_index,
               parent:               node.parent,
               name:                 node.name,
               depth:                node.depth,
               own_address_cells:    node.own_address_cells,
               own_size_cells:       node.own_size_cells,
               ranges:               node.ranges,
               compatible_list:      node.compatible_list,
               device_type:          node.device_type,
               reg:                  node.reg,
               interrupts_extended:  node.interrupts_extended,
               status:               node.status,
               interrupt_controller: node.interrupt_controller,
               interrupt_cells:      node.interrupt_cells,
               phandle:              node.phandle,
               linux_phandle:        node.linux_phandle,
               timebase_frequency:   node.timebase_frequency }
    }

    /// Whether this node has any child. The reader reports each node with its
    /// index, so leaf-ness is a query over the walk rather than a second pass
    /// through the token stream.
    fn is_leaf(&self, records: &[Record<'_>]) -> bool
    {
        !records.iter().any(|other| other.parent_index == Some(self.index))
    }
}

struct Controller
{
    hart:    u64,
    phandle: u32,
    enabled: bool,
    msip:    Option<u64>
}

/// Maps a reader refusal onto this bootloader's error, keeping the diagnosis
/// rather than flattening every malformed tree into one message.
fn tree_error(cause: mac_devicetree::Error) -> Error
{
    Error::Malformed(match cause
    {
        mac_devicetree::Error::Magic => "monitor blob is not a device tree",
        mac_devicetree::Error::Size => "monitor device tree total size is invalid",
        mac_devicetree::Error::Version => "monitor device tree version is unsupported",
        mac_devicetree::Error::Bounds => "monitor device tree block lies outside the blob",
        mac_devicetree::Error::Alignment => "monitor device tree block is not aligned",
        mac_devicetree::Error::Overlap => "monitor device tree blocks overlap",
        mac_devicetree::Error::Reservation => "monitor device tree reservation overflows",
        mac_devicetree::Error::Structure => "monitor device tree structure is malformed",
        mac_devicetree::Error::Strings => "monitor device tree string offset is invalid",
        mac_devicetree::Error::Cells => "monitor device tree cell declaration is invalid",
        mac_devicetree::Error::Cpu => "monitor device tree CPU node is not admissible",
        mac_devicetree::Error::DynamicReservation =>
        {
            "monitor device tree reserved-memory asks for dynamic allocation"
        }
    })
}

/// Derives this platform's monitor routes from the machine's tree.
///
/// A free function rather than a method: the reader is another crate's type, and
/// this platform's interpretation of it belongs beside the platform, not on the
/// reader.
///
/// Requires the already-admitted firmware heap. The returned routes are data
/// only; all publication, physical ownership and hart release are later.
pub fn monitor_routes(tree: &Tree<'_>, cpus: &[Cpu], clint: Range<u64>)
                      -> Result<MonitorRoutes>
{
        let first = cpus.first().ok_or(Error::Missing("monitor routes require enabled CPUs"))?;
        Topology::checked(cpus, first.hart).map_err(|_| {
                                               Error::Malformed("monitor routes require sorted \
                                                                 unique enabled CPUs")
                                           })?;
        if clint.start >= clint.end
           || !clint.start.is_multiple_of(4)
           || !clint.end.is_multiple_of(4)
        {
            return Err(Error::Malformed("monitor CLINT platform window is empty or unaligned"));
        }

        // Collect first, interpret after: children are reported before their
        // parents, so nothing about a container can be decided mid-walk.
        let mut records: Vec<Record<'_>> = Vec::new();
        let mut failure: Option<Error> = None;
        let walked = tree.for_each_node(|node| {
                          if failure.is_some()
                          {
                              return;
                          }
                          if let Err(error) = push(&mut records, Record::from(node))
                          {
                              failure = Some(error);
                          }
                      });
        if let Some(error) = failure
        {
            return Err(error);
        }
        walked.map_err(tree_error)?;

        // Root, /soc and /cpus, each exactly once.
        let root = records.iter()
                          .find(|record| record.parent_index.is_none())
                          .ok_or(Error::Missing("monitor DT has no root"))?;
        let mut socs = records.iter().filter(|record| record.depth == 1 && record.name == "soc");
        let soc = socs.next().ok_or(Error::Missing("monitor DT has no /soc"))?;
        if socs.next().is_some()
        {
            return Err(Error::Malformed("monitor DT repeats /soc"));
        }
        let mut cpu_containers =
            records.iter().filter(|record| record.depth == 1 && record.name == "cpus");
        let cpus_node = cpu_containers.next().ok_or(Error::Missing("monitor DT has no /cpus"))?;
        if cpu_containers.next().is_some()
        {
            return Err(Error::Malformed("monitor DT repeats /cpus"));
        }

        // The bus geometry this binding supports: an identity-mapped /soc whose
        // cells match the root's, declared explicitly on both.
        let root_geometry = geometry(root.own_address_cells, root.own_size_cells)?;
        let soc_geometry = geometry(soc.own_address_cells, soc.own_size_cells)?;
        if soc_geometry != root_geometry || soc.ranges != Some(&[][..])
        {
            return Err(Error::Malformed("monitor supports only equal-cell /soc identity mapping \
                                         with explicit empty ranges"));
        }
        if !enabled_of(soc.status)?
           || soc.compatible_list.is_none_or(|value| value != b"simple-bus\0")
        {
            return Err(Error::Malformed("monitor requires an enabled /soc simple-bus"));
        }

        // The CPU container: one or two address cells, no size cells, and the
        // counter rate it states for itself.
        let cpu_width = match (cpus_node.own_address_cells, cpus_node.own_size_cells)
        {
            (Some(width @ (1 | 2)), Some(0)) => width,
            _ =>
            {
                return Err(Error::Malformed("/cpus requires one or two address cells and zero \
                                             size cells"));
            }
        };
        let cpus_enabled = enabled_of(cpus_node.status)?;
        let timebase_hz =
            cpus_node.timebase_frequency
                     .ok_or(Error::Missing("monitor /cpus has no timebase-frequency"))?;
        if timebase_hz == 0 || u64::from(timebase_hz) > u64::MAX / 2
        {
            return Err(Error::Malformed("monitor timebase-frequency is zero or outside counter \
                                         arithmetic"));
        }

        // Every CPU under /cpus, with the interrupt controller that belongs to
        // it. Identity is by index, not by name: two CPUs may share a name and so
        // may their controllers.
        let mut controllers: Vec<Controller> = Vec::new();
        for record in records.iter().filter(|record| record.parent_index == Some(cpus_node.index))
        {
            let named = record.name == "cpu" || record.name.starts_with("cpu@");
            if !named && record.device_type != Some("cpu")
            {
                continue;
            }
            if record.device_type != Some("cpu")
            {
                return Err(Error::Malformed("monitor CPU requires device_type cpu"));
            }
            let reg = record.reg.ok_or(Error::Missing("monitor CPU has no reg"))?;
            if reg.len() != cpu_width as usize * 4
            {
                return Err(Error::Malformed("monitor CPU reg must contain exactly one hart ID"));
            }
            let hart = cells(reg, cpu_width)?;
            let enabled = cpus_enabled && enabled_of(record.status)?;

            let mut controller: Option<u32> = None;
            for child in records.iter().filter(|other| other.parent_index == Some(record.index))
            {
                let named = child.name == "interrupt-controller"
                            || child.name.starts_with("interrupt-controller@");
                let known = child.compatible_list == Some(&b"riscv,cpu-intc\0"[..]);
                if !named && !known
                {
                    continue;
                }
                if !known || !child.interrupt_controller || child.interrupt_cells != Some(1)
                {
                    return Err(Error::Malformed("monitor requires riscv,cpu-intc with empty \
                                                 interrupt-controller and one interrupt cell"));
                }
                let handle = child.phandle
                                  .or(child.linux_phandle)
                                  .ok_or(Error::Missing("monitor CPU interrupt controller has no \
                                                         phandle"))?;
                if enabled && !enabled_of(child.status)?
                {
                    return Err(Error::Malformed("monitor CPU interrupt controller is disabled"));
                }
                if !child.is_leaf(&records)
                {
                    return Err(Error::Malformed("monitor CPU interrupt controller must be a \
                                                 leaf"));
                }
                once(&mut controller, handle)?;
            }
            push(&mut controllers,
                 Controller { hart,
                              phandle:
                                  controller.ok_or(Error::Missing("monitor CPU has no interrupt \
                                                                   controller"))?,
                              enabled,
                              msip: None })?;
        }

        // The legacy CLINT, directly under /soc. A CLINT-compatible node
        // anywhere else is a translated bus, which this binding does not
        // support.
        let mut selected: Option<&[u8]> = None;
        for record in &records
        {
            if let Some(list) = record.compatible_list
            {
                let clint_compatible = contains_compatible(list, b"sifive,clint0")?
                                        || contains_compatible(list, b"riscv,clint0")?;
                // Directly under /soc, which is what "no translated bus" means.
                // The parent's name is the check; a depth constant would be a
                // second, weaker statement of the same thing.
                if clint_compatible && record.parent != Some("soc")
                {
                    return Err(Error::Malformed("monitor CLINT must be a direct /soc child; \
                                                 translated buses are unsupported"));
                }
            }
            let named = record.name == "clint" || record.name.starts_with("clint@");
            let known = record.compatible_list.is_some_and(legacy_clint);
            if !named && !known
            {
                continue;
            }
            if !known || !enabled_of(record.status)?
            {
                return Err(Error::Malformed("monitor supports only an enabled legacy QEMU \
                                             CLINT"));
            }
            let reg = record.reg.ok_or(Error::Missing("monitor CLINT has no reg"))?;
            if reg.len() != ((soc_geometry.0 + soc_geometry.1) * 4) as usize
            {
                return Err(Error::Malformed("monitor CLINT reg must be one address/size tuple"));
            }
            let start = cells(reg, soc_geometry.0)?;
            let bytes = cells(&reg[(soc_geometry.0 * 4) as usize..], soc_geometry.1)?;
            let end = start.checked_add(bytes)
                           .ok_or(Error::Malformed("monitor CLINT reg overflows"))?;
            if bytes == 0 || start != clint.start || end != clint.end
            {
                return Err(Error::Malformed("monitor CLINT reg does not match the selected \
                                             platform window"));
            }
            if !record.is_leaf(&records)
            {
                return Err(Error::Malformed("monitor legacy CLINT must be a leaf device"));
            }
            let interrupts =
                record.interrupts_extended
                      .ok_or(Error::Missing("monitor CLINT has no interrupts-extended"))?;
            once(&mut selected, interrupts)?;
        }
        let interrupts =
            selected.ok_or(Error::Missing("monitor found no selected legacy /soc CLINT"))?;

        // Phandles: aliases must agree, reserved values are refused, and no
        // handle may name two nodes. This is an interrupt-controller concept, so
        // it lives here rather than in the shared reader.
        let mut handles: Vec<u32> = Vec::new();
        for record in &records
        {
            let standard = record.phandle;
            let linux = record.linux_phandle;
            if let (Some(standard), Some(linux)) = (standard, linux)
               && standard != linux
            {
                return Err(Error::Malformed("monitor DT phandle aliases disagree"));
            }
            if let Some(value) = standard.or(linux)
            {
                if value == 0 || value == u32::MAX
                {
                    return Err(Error::Malformed("monitor DT uses a reserved phandle"));
                }
                push(&mut handles, value)?;
            }
        }
        handles.sort_unstable();
        if handles.windows(2).any(|pair| pair[0] == pair[1])
        {
            return Err(Error::Malformed("monitor DT repeats a phandle across nodes"));
        }

        // The admitted topology and the tree's enabled CPUs must be the same set.
        controllers.sort_unstable_by_key(|cpu| cpu.hart);
        if controllers.windows(2).any(|pair| pair[0].hart == pair[1].hart)
        {
            return Err(Error::Malformed("monitor CPU nodes repeat a hart ID"));
        }
        let mut enabled = controllers.iter().filter(|cpu| cpu.enabled);
        for expected in cpus
        {
            if enabled.next().is_none_or(|actual| actual.hart != expected.hart)
            {
                return Err(Error::Malformed("monitor DT enabled CPUs differ from admitted \
                                             topology"));
            }
        }
        if enabled.next().is_some()
        {
            return Err(Error::Malformed("monitor DT has an extra enabled CPU"));
        }

        // The CLINT's interrupt tuples name each CPU's controller and the two
        // causes this binding accepts, in hart order.
        controllers.sort_unstable_by_key(|cpu| cpu.phandle);
        if interrupts.is_empty() || !interrupts.len().is_multiple_of(16)
        {
            return Err(Error::Malformed("monitor CLINT requires ordered paired software/timer \
                                         interrupt tuples"));
        }
        for (ordinal, pair) in interrupts.chunks_exact(16).enumerate()
        {
            let target = word(pair, 0)?;
            if target != word(pair, 8)? || word(pair, 4)? != 3 || word(pair, 12)? != 7
            {
                return Err(Error::Malformed("monitor CLINT pair must use one CPU phandle with \
                                             ordered causes 3 and 7"));
            }
            let index = controllers.binary_search_by_key(&target, |cpu| cpu.phandle)
                                   .map_err(|_| {
                                       Error::Malformed("monitor CLINT route references a foreign \
                                                         CPU interrupt controller")
                                   })?;
            let offset = u64::try_from(ordinal).ok()
                                               .and_then(|n| n.checked_mul(4))
                                               .ok_or(Error::Malformed("monitor CLINT MSIP ordinal \
                                                                        overflows"))?;
            let msip = clint.start
                            .checked_add(offset)
                            .ok_or(Error::Malformed("monitor CLINT MSIP address overflows"))?;
            // Legacy MSIP occupies only the prefix before MTIMECMP. The full
            // advertised CLINT span is not permission to address timer bytes.
            if offset.checked_add(4).is_none_or(|end| end > 0x4000)
               || msip.checked_add(4).is_none_or(|end| end > clint.end)
            {
                return Err(Error::Malformed("monitor CLINT route exceeds the legacy MSIP prefix"));
            }
            once(&mut controllers[index].msip, msip)?;
        }

        controllers.sort_unstable_by_key(|cpu| cpu.hart);
        let mut routes = Vec::new();
        routes.try_reserve_exact(cpus.len())
              .map_err(|_| Error::AllocationFailed("monitor route allocation failed"))?;
        for cpu in controllers.into_iter().filter(|cpu| cpu.enabled)
        {
            routes.push(MonitorRoute { hart:   cpu.hart,
                                       signal: cpu.msip
                                                   .ok_or(Error::Missing("monitor enabled CPU has \
                                                                          no CLINT route"))? });
        }
    Ok(MonitorRoutes { timebase_hz: u64::from(timebase_hz),
                       routes })
}
