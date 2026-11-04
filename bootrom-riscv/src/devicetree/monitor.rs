//! Data-only admission for the selected QEMU legacy CLINT binding. No MMIO or
//! hart release occurs here. Numeric hart IDs never determine register offsets.
#![forbid(unsafe_code)]
use alloc::vec::Vec;
use core::ops::Range;

use mac_abi::boot::topology::{Cpu, Topology};

use super::{Error, Event, Result, Tree, cells, cpu_cells, set_cpu_cell, set_cpu_status, word};

pub struct MonitorRoute
{
    pub hart: u64,
    pub msip: u64
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
fn scalar(value: &[u8]) -> Result<u32>
{
    if value.len() != 4
    {
        return Err(Error::Malformed("monitor DT scalar must be one u32"));
    }
    word(value, 0)
}
fn status(slot: &mut Option<bool>, value: &[u8]) -> Result<()>
{
    set_cpu_status(slot, value)
}
fn phandle(value: &[u8]) -> Result<u32>
{
    let value = scalar(value)?;
    if value == 0 || value == u32::MAX
    {
        return Err(Error::Malformed("monitor DT uses a reserved phandle"));
    }
    Ok(value)
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

#[derive(Default)]
struct Phandles
{
    standard: Option<u32>,
    linux:    Option<u32>,
    values:   Vec<u32>
}
impl Phandles
{
    // Properties precede children, so one pending property group suffices; no
    // recursive node stack or separately issued node identity is necessary.
    fn finish_properties(&mut self) -> Result<()>
    {
        let standard = self.standard.take();
        let linux = self.linux.take();
        if let (Some(standard), Some(linux)) = (standard, linux)
           && standard != linux
        {
            return Err(Error::Malformed("monitor DT phandle aliases disagree"));
        }
        if let Some(value) = standard.or(linux)
        {
            push(&mut self.values, value)?;
        }
        Ok(())
    }

    fn validate(mut self) -> Result<()>
    {
        self.values.sort_unstable();
        if self.values.windows(2).any(|pair| pair[0] == pair[1])
        {
            return Err(Error::Malformed("monitor DT repeats a phandle across nodes"));
        }
        Ok(())
    }
}
#[derive(Default)]
struct Bus<'a>
{
    address:    Option<u32>,
    size:       Option<u32>,
    ranges:     Option<&'a [u8]>,
    compatible: Option<&'a [u8]>,
    status:     Option<bool>
}
impl Bus<'_>
{
    fn geometry(&self) -> Result<(u32, u32)>
    {
        match (self.address, self.size)
        {
            (Some(a @ (1 | 2)), Some(s @ (1 | 2))) => Ok((a, s)),
            _ =>
            {
                Err(Error::Malformed("monitor bus requires explicit one-or-two address and size \
                                      cells"))
            }
        }
    }

    fn identity(&self, root: &Self) -> Result<(u32, u32)>
    {
        let geometry = self.geometry()?;
        if geometry != root.geometry()? || self.ranges != Some(&[][..])
        {
            return Err(Error::Malformed("monitor supports only equal-cell /soc identity \
                                         mapping with explicit empty ranges"));
        }
        if !self.status.unwrap_or(true)
           || self.compatible.is_none_or(|value| value != b"simple-bus\0")
        {
            return Err(Error::Malformed("monitor requires an enabled /soc simple-bus"));
        }
        Ok(geometry)
    }
}
#[derive(Default)]
struct CpuNode<'a>
{
    named:       bool,
    device_type: Option<&'a [u8]>,
    reg:         Option<&'a [u8]>,
    status:      Option<bool>,
    controller:  Option<u32>
}
#[derive(Default)]
struct Intc<'a>
{
    named:                bool,
    compatible:           Option<&'a [u8]>,
    interrupt_controller: Option<()>,
    cells:                Option<u32>,
    phandle:              Option<u32>,
    linux_phandle:        Option<u32>,
    status:               Option<bool>
}
impl Intc<'_>
{
    fn finish(&self, cpu_enabled: bool) -> Result<Option<u32>>
    {
        let known = self.compatible == Some(&b"riscv,cpu-intc\0"[..]);
        if !self.named && !known
        {
            return Ok(None);
        }
        if !known || self.interrupt_controller.is_none() || self.cells != Some(1)
        {
            return Err(Error::Malformed("monitor requires riscv,cpu-intc with empty \
                                         interrupt-controller and one interrupt cell"));
        }
        let phandle =
            self.phandle
                .or(self.linux_phandle)
                .ok_or(Error::Missing("monitor CPU interrupt controller has no phandle"))?;
        if cpu_enabled && !self.status.unwrap_or(true)
        {
            return Err(Error::Malformed("monitor CPU interrupt controller is disabled"));
        }
        Ok(Some(phandle))
    }
}
struct Controller
{
    hart:    u64,
    phandle: u32,
    enabled: bool,
    msip:    Option<u64>
}
#[derive(Default)]
struct Clint<'a>
{
    named:      bool,
    compatible: Option<&'a [u8]>,
    reg:        Option<&'a [u8]>,
    interrupts: Option<&'a [u8]>,
    status:     Option<bool>
}
impl<'a> Clint<'a>
{
    fn finish(&self, root: &Bus<'_>, soc: &Bus<'_>, allowed: &Range<u64>)
              -> Result<Option<&'a [u8]>>
    {
        let known = self.compatible.is_some_and(legacy_clint);
        if !self.named && !known
        {
            return Ok(None);
        }
        if !known || !self.status.unwrap_or(true)
        {
            return Err(Error::Malformed("monitor supports only an enabled legacy QEMU CLINT"));
        }
        let (address_cells, size_cells) = soc.identity(root)?;
        let reg = self.reg.ok_or(Error::Missing("monitor CLINT has no reg"))?;
        if reg.len() != ((address_cells + size_cells) * 4) as usize
        {
            return Err(Error::Malformed("monitor CLINT reg must be one address/size tuple"));
        }
        let start = cells(reg, address_cells)?;
        let bytes = cells(&reg[(address_cells * 4) as usize..], size_cells)?;
        let end =
            start.checked_add(bytes).ok_or(Error::Malformed("monitor CLINT reg overflows"))?;
        if bytes == 0 || start != allowed.start || end != allowed.end
        {
            return Err(Error::Malformed("monitor CLINT reg does not match the selected \
                                         platform window"));
        }
        Ok(Some(self.interrupts
                    .ok_or(Error::Missing("monitor CLINT has no interrupts-extended"))?))
    }
}

impl Tree<'_>
{
    /// Requires the already-admitted firmware heap. The returned routes are
    /// data only; all publication, physical ownership and hart release are later.
    pub fn monitor_routes(&self, cpus: &[Cpu], clint: Range<u64>) -> Result<MonitorRoutes>
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
        let mut scanner = self.scanner();
        let mut root = Bus::default();
        let mut soc = Bus::default();
        let (mut in_cpus, mut in_soc, mut cpus_seen, mut soc_seen) = (false, false, false, false);
        let (mut cpu_address, mut cpu_size) = (None, None);
        let mut cpus_status = None;
        let mut timebase = None;
        let mut cpu = CpuNode::default();
        let mut intc = Intc::default();
        let mut device = Clint::default();
        let mut selected = None;
        let mut controllers = Vec::new();
        let mut phandles = Phandles::default();
        let mut properties_allowed = false;
        loop
        {
            match scanner.next()?
            {
                Event::Begin { name,
                               depth } =>
                {
                    phandles.finish_properties()?;
                    properties_allowed = true;
                    if depth == 2
                    {
                        in_cpus = name == b"cpus";
                        in_soc = name == b"soc";
                        if in_cpus
                        {
                            if cpus_seen
                            {
                                return Err(Error::Malformed("monitor DT repeats /cpus"));
                            }
                            cpus_seen = true;
                        }
                        if in_soc
                        {
                            if soc_seen
                            {
                                return Err(Error::Malformed("monitor DT repeats /soc"));
                            }
                            soc_seen = true;
                        }
                    }
                    if in_cpus && depth == 3
                    {
                        cpu_cells(cpu_address, cpu_size)?;
                        cpu = CpuNode { named: name == b"cpu" || name.starts_with(b"cpu@"),
                                        ..Default::default() };
                    }
                    if in_cpus && depth == 4
                    {
                        intc = Intc { named: name == b"interrupt-controller"
                                             || name.starts_with(b"interrupt-controller@"),
                                      ..Default::default() };
                    }
                    if in_cpus
                       && depth == 5
                       && (intc.named || intc.compatible == Some(&b"riscv,cpu-intc\0"[..]))
                    {
                        return Err(Error::Malformed("monitor CPU interrupt controller must be \
                                                     a leaf"));
                    }
                    if in_soc
                       && depth == 4
                       && (device.named || device.compatible.is_some_and(legacy_clint))
                    {
                        return Err(Error::Malformed("monitor legacy CLINT must be a leaf device"));
                    }
                    if in_soc && depth == 3
                    {
                        device = Clint { named: name == b"clint" || name.starts_with(b"clint@"),
                                         ..Default::default() };
                    }
                }
                Event::End { depth } =>
                {
                    phandles.finish_properties()?;
                    properties_allowed = false;
                    if in_cpus
                       && depth == 4
                       && let Some(phandle) =
                           intc.finish(cpus_status.unwrap_or(true) && cpu.status.unwrap_or(true))?
                    {
                        once(&mut cpu.controller, phandle)?;
                    }
                    if in_cpus
                       && depth == 3
                       && (cpu.named || cpu.device_type == Some(&b"cpu\0"[..]))
                    {
                        if cpu.device_type != Some(&b"cpu\0"[..])
                        {
                            return Err(Error::Malformed("monitor CPU requires device_type cpu"));
                        }
                        let width = cpu_cells(cpu_address, cpu_size)?;
                        let reg = cpu.reg.ok_or(Error::Missing("monitor CPU has no reg"))?;
                        if reg.len() != width as usize * 4
                        {
                            return Err(Error::Malformed("monitor CPU reg must contain exactly \
                                                         one hart ID"));
                        }
                        push(&mut controllers,
                             Controller { hart:    cells(reg, width)?,
                                          phandle:
                                              cpu.controller
                                                 .ok_or(Error::Missing("monitor CPU has no \
                                                                        interrupt controller"))?,
                                          enabled: cpus_status.unwrap_or(true)
                                                   && cpu.status.unwrap_or(true),
                                          msip:    None })?;
                    }
                    if in_soc
                       && depth == 3
                       && let Some(interrupts) = device.finish(&root, &soc, &clint)?
                    {
                        once(&mut selected, interrupts)?;
                    }
                    if depth == 2
                    {
                        in_cpus = false;
                        in_soc = false;
                    }
                }
                Event::Property { name,
                                  value,
                                  depth } =>
                {
                    if !properties_allowed
                    {
                        return Err(Error::Malformed("monitor DT properties must precede child \
                                                     nodes"));
                    }
                    match name
                    {
                        b"phandle" => once(&mut phandles.standard, phandle(value)?)?,
                        b"linux,phandle" => once(&mut phandles.linux, phandle(value)?)?,
                        b"compatible" =>
                        {
                            if (contains_compatible(value, b"sifive,clint0")?
                                || contains_compatible(value, b"riscv,clint0")?)
                               && !(in_soc && depth == 3)
                            {
                                return Err(Error::Malformed("monitor CLINT must be a direct \
                                                             /soc child; translated buses are \
                                                             unsupported"));
                            }
                        }
                        _ => ()
                    }
                    if depth == 1
                    {
                        match name
                        {
                            b"#address-cells" => set_cpu_cell(&mut root.address, value)?,
                            b"#size-cells" => set_cpu_cell(&mut root.size, value)?,
                            _ => ()
                        }
                    }
                    if in_soc && depth == 2
                    {
                        match name
                        {
                            b"#address-cells" => set_cpu_cell(&mut soc.address, value)?,
                            b"#size-cells" => set_cpu_cell(&mut soc.size, value)?,
                            b"ranges" => once(&mut soc.ranges, value)?,
                            b"compatible" => once(&mut soc.compatible, value)?,
                            b"status" => status(&mut soc.status, value)?,
                            _ => ()
                        }
                    }
                    if in_cpus && depth == 2
                    {
                        match name
                        {
                            b"#address-cells" => set_cpu_cell(&mut cpu_address, value)?,
                            b"#size-cells" => set_cpu_cell(&mut cpu_size, value)?,
                            b"status" => status(&mut cpus_status, value)?,
                            b"timebase-frequency" =>
                            {
                                let hz = u64::from(scalar(value)?);
                                if hz == 0 || hz > u64::MAX / 2
                                {
                                    return Err(Error::Malformed("monitor timebase-frequency is \
                                                                 zero or outside counter \
                                                                 arithmetic"));
                                }
                                once(&mut timebase, hz)?;
                            }
                            _ => ()
                        }
                    }
                    if in_cpus && depth == 3
                    {
                        match name
                        {
                            b"device_type" => once(&mut cpu.device_type, value)?,
                            b"reg" => once(&mut cpu.reg, value)?,
                            b"status" => status(&mut cpu.status, value)?,
                            _ => ()
                        }
                    }
                    if in_cpus && depth == 4
                    {
                        match name
                        {
                            b"compatible" => once(&mut intc.compatible, value)?,
                            b"interrupt-controller" =>
                            {
                                if !value.is_empty()
                                {
                                    return Err(Error::Malformed("monitor interrupt-controller \
                                                                 flag must be empty"));
                                }
                                once(&mut intc.interrupt_controller, ())?;
                            }
                            b"#interrupt-cells" => once(&mut intc.cells, scalar(value)?)?,
                            b"phandle" => once(&mut intc.phandle, phandle(value)?)?,
                            b"linux,phandle" => once(&mut intc.linux_phandle, phandle(value)?)?,
                            b"status" => status(&mut intc.status, value)?,
                            _ => ()
                        }
                    }
                    if in_soc && depth == 3
                    {
                        match name
                        {
                            b"compatible" => once(&mut device.compatible, value)?,
                            b"reg" => once(&mut device.reg, value)?,
                            b"interrupts-extended" => once(&mut device.interrupts, value)?,
                            b"status" => status(&mut device.status, value)?,
                            _ => ()
                        }
                    }
                }
                Event::Nop => (),
                Event::Finish =>
                {
                    if !scanner.complete()
                    {
                        return Err(Error::Malformed("monitor DT structure is incomplete or has \
                                                     trailing tokens"));
                    }
                    break;
                }
            }
        }
        phandles.validate()?;
        let timebase_hz =
            timebase.ok_or(Error::Missing("monitor /cpus has no timebase-frequency"))?;
        let interrupts =
            selected.ok_or(Error::Missing("monitor found no selected legacy /soc CLINT"))?;
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
            routes.push(MonitorRoute { hart: cpu.hart,
                                       msip: cpu.msip
                                                .ok_or(Error::Missing("monitor enabled CPU has no \
                                                                       CLINT route"))? });
        }
        Ok(MonitorRoutes { timebase_hz,
                           routes })
    }
}
