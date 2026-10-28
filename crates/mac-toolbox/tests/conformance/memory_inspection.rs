//! Real native mapping observations through the ordinary public SDK.
use core::fmt::Write;

use mac_toolbox::{Error, System,
                  inspection::{Region, RegionKind}};

use crate::{LogBuffer, conformance::Failure};
fn require(value: bool, message: &'static str) -> Result<(), Failure>
{
    if value { Ok(()) } else { Err(Failure::Mismatch(message)) }
}
fn service(error: mac_toolbox::inspection::Failure) -> Failure
{
    Failure::Service(error.cause.status())
}
pub fn run(system: &System<'_>) -> Result<(), Failure>
{
    let memory = system.inspection().system_memory().map_err(service)?;
    let summary = memory.summary();
    require(summary.shared_supervisor && summary.accounting_unavailable,
            "truthful shared-supervisor/accounting coverage")?;
    require(summary.region_count > 0
            && summary.heap_used > 0
            && summary.heap_used <= summary.heap_capacity,
            "real native heap counters")?;
    let own = system.inspection()
                    .snapshot(mac_toolbox::inspection::Scope::Own)
                    .map_err(service)?
                    .at(0)
                    .map_err(service)?;
    require(own.incarnation == system.processes().current()?.id.0,
            "account row has genuine current identity")?;
    let mac_toolbox::inspection::MemoryAccounting::ExecutionAndResidentHeap { kernel,
                                                                              application,
                                                                              closing } =
        own.accounting
    else
    {
        return Err(Failure::Mismatch("actual process execution account unavailable"));
    };
    require(!closing
            && kernel.limit == 16 * 1024 * 1024
            && application.limit == 64 * 1024 * 1024
            && kernel.reserved == 0
            && application.reserved == 0
            && application.committed == 4 * 1024 * 1024
            && kernel.committed > 0
            && kernel.committed >= u64::from(summary.page_bytes),
            "configured limits, actual stack and resident heap charges")?;
    let mut account_line = LogBuffer::new();
    let _ = writeln!(account_line,
                     "MEMORY_EXECUTION_ACCOUNT app={} kernel_limit={} application_limit={} \
                      kernel_committed={} application_committed={} \
                      coverage=execution-resident-heap",
                     own.incarnation,
                     kernel.limit,
                     application.limit,
                     kernel.committed,
                     application.committed);
    system.log(account_line.text())?;
    // This scenario starts before this owner's Memory Manager zone is backed.
    // Growing its actual resident Vec must appear in this account. Disposal
    // leaves reusable zone capacity until the domain's real teardown.
    let mut handle = system.memory().handle_from_bytes(b"original account")?;
    handle.resize(64 * 1024)?;
    let mut contents = [0; 16];
    require(handle.read(0, &mut contents)? == 16 && &contents == b"original account",
            "resident realloc preserved contents")?;
    require(handle.resize(2 * 1024 * 1024) == Err(Error::MemoryFull)
            && handle.len()? == 64 * 1024,
            "bounded handle refusal preserves original backing and remains usable")?;
    let row = system.inspection()
                    .snapshot(mac_toolbox::inspection::Scope::Own)
                    .map_err(service)?
                    .at(0)
                    .map_err(service)?;
    let mac_toolbox::inspection::MemoryAccounting::ExecutionAndResidentHeap { kernel:
                                                                                  allocated,
                                                                              application:
                                                                                  same_stack,
                                                                              .. } = row.accounting
    else
    {
        return Err(Failure::Mismatch("resident account disappeared"));
    };
    require(allocated.committed > kernel.committed + 60 * 1024 && same_stack == application,
            "resident zone backing and headers charged to actual owner, not System or stack")?;
    handle.dispose()?;
    system.log("MEMORY_RESIDENT_ACCOUNT zone=original-owner realloc=preserved refusal=usable \
                stack=unchanged disposal=passed\n")?;
    let mut previous = None;
    let mut count = 0;
    let mut stack = None::<Region>;
    let mut lower = None;
    let mut upper = None;
    let mut heap = 0;
    let mut direct = 0;
    let mut code = false;
    let mut guard_count = 0;
    for row in memory.regions()
    {
        let row = row.map_err(service)?;
        count += 1;
        require(previous.is_none_or(|end| row.virtual_base >= end),
                "mapping rows sorted and nonoverlapping")?;
        previous = row.virtual_base.checked_add(row.bytes);
        require(!row.protection.user, "current shared mappings are supervisor-only")?;
        match row.kind
        {
            RegionKind::SystemHeap =>
            {
                require(row.protection.read && row.protection.write && !row.protection.execute,
                        "heap RW/NX")?;
                heap += row.bytes;
            }
            RegionKind::SystemStack =>
            {
                require(stack.is_none(), "single real hart0 stack")?;
                stack = Some(row);
            }
            RegionKind::Guard =>
            {
                require(row.physical_base.is_none(), "guard has no backing mapping")?;
                if lower.is_none()
                {
                    lower = Some(row);
                }
                else
                {
                    upper = Some(row);
                }
                guard_count += 1;
            }
            RegionKind::SystemCode =>
            {
                require(row.protection.execute && !row.protection.write, "resident code RX")?;
                code = true;
            }
            RegionKind::DirectMap =>
            {
                direct += row.bytes;
            }
            _ =>
            {}
        }
    }
    require(count == summary.region_count && guard_count == 2,
            "all actual mapping batches and both guards enumerated")?;
    let stack = stack.ok_or(Failure::Mismatch("System stack missing"))?;
    require(lower.is_some_and(|r| r.virtual_base + r.bytes == stack.virtual_base)
            && upper.is_some_and(|r| r.virtual_base == stack.virtual_base + stack.bytes),
            "unmapped guards bracket real stack")?;
    // This existing guest scenario checks the selected QemuRISC-V defaults:
    // mapped reserve bytes must never be advertised as ordinary allocator space.
    require(heap == 512 * 1024 * 1024
            && summary.heap_capacity == 480 * 1024 * 1024
            && heap - summary.heap_capacity == 32 * 1024 * 1024
            && direct == summary.ram_bytes
            && code,
            "mapped heap includes protected floor; ordinary capacity excludes it")?;
    for (left, right) in memory.regions().zip(memory.regions())
    {
        require(left.map_err(service)? == right.map_err(service)?,
                "immutable mapping reads are repeatable")?;
    }
    let bad = memory.batch(summary.region_count)
                    .err()
                    .ok_or(Failure::Mismatch("out-of-range mapping read succeeded"))?;
    let mut detail = LogBuffer::new();
    let _ = write!(detail, "{bad}");
    require(bad.cause == Error::NotFound
            && detail.text().contains("outside this System mapping snapshot"),
            "invalid mapping position has informative typed refusal")?;
    require(memory.batch(0).map_err(service)?.at(0).is_some(),
            "refused read leaves snapshot usable")?;
    let mut line = LogBuffer::new();
    let _ = writeln!(line,
                     "MEMORY_SNAPSHOT root={:#x} rows={} ram={} page={} heap_used={} \
                      heap_capacity={} mapped_heap={} free_pages={} stack={} guards={}",
                     summary.root_physical,
                     summary.region_count,
                     summary.ram_bytes,
                     summary.page_bytes,
                     summary.heap_used,
                     summary.heap_capacity,
                     heap,
                     summary.free_pages,
                     stack.bytes,
                     guard_count);
    system.log(line.text())?;
    Ok(())
}
