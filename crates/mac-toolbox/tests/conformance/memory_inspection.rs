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
    let mut stack_count = 0u64;
    let mut stack_bytes = 0u64;
    let mut lower = None::<Region>;
    let mut upper = None::<u64>;
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
        require(lower.is_none() || row.kind == RegionKind::SystemStack,
                "lower service guard immediately precedes its stack")?;
        require(upper.is_none_or(|base| {
                         row.kind == RegionKind::Guard && row.virtual_base == base
                     }),
                "upper service guard immediately follows its stack")?;
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
                require(row.bytes == 4 * 1024 * 1024
                        && row.physical_base.is_some_and(|base| base % summary.page_bytes == 0)
                        && row.protection.read
                        && row.protection.write
                        && !row.protection.execute
                        && row.protection.global,
                        "real configured service stack is backed supervisor global RW/NX")?;
                require(lower.take().is_some_and(|guard| {
                                        guard.virtual_base.checked_add(guard.bytes)
                                        == Some(row.virtual_base)
                                    }),
                        "unmapped lower guard brackets each service stack")?;
                upper = row.virtual_base.checked_add(row.bytes);
                require(upper.is_some(), "service stack upper bound is representable")?;
                stack_count = stack_count.checked_add(1)
                                         .ok_or(Failure::Mismatch("service stack count overflow"))?;
                stack_bytes = stack_bytes.checked_add(row.bytes)
                                         .ok_or(Failure::Mismatch("service stack sum overflow"))?;
            }
            RegionKind::Guard =>
            {
                require(row.physical_base.is_none()
                        && row.bytes == summary.page_bytes
                        && !row.protection.read
                        && !row.protection.write
                        && !row.protection.execute
                        && !row.protection.global,
                        "each service guard is one unmapped page with no protection")?;
                if upper.take().is_none()
                {
                    lower = Some(row);
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
    require(count == summary.region_count
            && stack_count > 0
            && stack_count.checked_mul(2) == Some(guard_count)
            && lower.is_none()
            && upper.is_none(),
            "every observed service stack has exactly two real unmapped guards")?;
    // The SDK exposes mapped owners, not a topology query. The capture report
    // compares this observed count with its actual shipping machine configuration.
    // These counters remain separate attribution samples, never a whole-System
    // usage sum or a promise of an atomic machine-wide snapshot.
    require(summary.ordinary_foundation_heap_bytes > 0
            && summary.ordinary_foundation_heap_bytes <= summary.heap_capacity
            && summary.tracked_foundation_physical_bytes >= stack_bytes
            && summary.tracked_foundation_physical_bytes <= summary.ram_bytes
            && summary.tracked_foundation_physical_bytes % summary.page_bytes == 0,
            "Foundation heap occupancy and tracked physical leases are separately bounded")?;
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
                      heap_capacity={} mapped_heap={} free_pages={} stack={} stacks={} \
                      stack_total={} guards={}",
                     summary.root_physical,
                     summary.region_count,
                     summary.ram_bytes,
                     summary.page_bytes,
                     summary.heap_used,
                     summary.heap_capacity,
                     heap,
                     summary.free_pages,
                     4 * 1024 * 1024,
                     stack_count,
                     stack_bytes,
                     guard_count);
    system.log(line.text())?;
    let mut attribution = LogBuffer::new();
    let _ = writeln!(attribution,
                     "MEMORY_FOUNDATION ordinary_heap_bytes={} tracked_physical_bytes={} \
                      physical_scope=service-storage accounting=partial",
                     summary.ordinary_foundation_heap_bytes,
                     summary.tracked_foundation_physical_bytes);
    system.log(attribution.text())?;
    Ok(())
}
