use mac_abi::{BootMemoryRange, boot::Memory};
use mac_kernel_model::{pages::{PhysicalAddress, VirtualAddress,
                               physical::{DirectMap, Error}},
                       policy::AddressRange};
use mac_riscv_pages::Sv39;
fn range(base: u64, length: u64) -> BootMemoryRange
{
    BootMemoryRange { base,
                      length }
}
fn window() -> AddressRange
{
    AddressRange::new(0xffffffe000000000, 0xfffffff000000000).unwrap()
}
#[test]
fn sparse_unsorted_ram_maps_only_real_banks_and_keeps_physical_addresses_distinct()
{
    let ram = [range(0x90000000, 4096), range(0x80001000, 4096), range(0x80000000, 4096)];
    let reserved = [ram[2]];
    let memory = Memory { ram:          &ram,
                          reservations: &reserved }.checked(4096, ram[0])
                                                   .unwrap();
    let map = DirectMap::new::<Sv39>(memory, window(), PhysicalAddress(0)).unwrap();
    let addresses: Vec<_> = map.banks().collect();
    assert_eq!(addresses.len(), 3, "construction enumerated physical pages rather than banks");
    assert_eq!(addresses[0].physical, PhysicalAddress(0x90000000));
    assert_eq!(addresses[0].virtual_address, VirtualAddress(window().start() + 0x90000000));
    // The direct map includes pinned RAM too, but never considers a hole RAM.
    let cpu = map.virtual_address(PhysicalAddress(ram[2].base), 8192).unwrap();
    assert_eq!(cpu, VirtualAddress(window().start() + 0x80000000));
    assert_eq!(map.physical_address(cpu, 8192).unwrap(), PhysicalAddress(0x80000000));
    assert!(matches!(map.virtual_address(PhysicalAddress(0x80002000), 1),
                     Err(Error::UnmappedRam { physical: 0x80002000,
                                              .. })));
    assert!(map.physical_address(VirtualAddress(window().start() + 0x80002000), 1).is_err());
    assert!(matches!(map.virtual_address(PhysicalAddress(0x80001000), 4097),
                     Err(Error::UnmappedRam { .. })));
    let message = map.virtual_address(PhysicalAddress(0x80002000), 4096).unwrap_err().to_string();
    assert!(message.contains("0x80002000+0x1000") && message.contains("discovered RAM"),
            "{message}");
}
#[test]
fn high_physical_origin_uses_subtraction_before_virtual_addition()
{
    let base = 1u64 << 55;
    let ram = [range(base + 4096, 8192)];
    let memory = Memory { ram:          &ram,
                          reservations: &[] }.checked(4096, ram[0])
                                             .unwrap();
    let map = DirectMap::new::<Sv39>(memory, window(), PhysicalAddress(base)).unwrap();
    let extent = map.banks().next().unwrap();
    assert_eq!(extent.virtual_address, VirtualAddress(window().start() + 4096));
    assert_eq!(map.virtual_address(PhysicalAddress(base + 4096), 8192).unwrap(),
               extent.virtual_address);
    assert_eq!(map.physical_address(extent.virtual_address, 8192).unwrap(),
               PhysicalAddress(base + 4096));
}
#[test]
fn range_geometry_holes_and_overflow_refuse_without_producing_cpu_pointers()
{
    let ram = [range(0x80000000, 8 << 30)];
    let memory = Memory { ram:          &ram,
                          reservations: &[] }.checked(4096, ram[0])
                                             .unwrap();
    let make = |region, origin| DirectMap::new::<Sv39>(memory, region, PhysicalAddress(origin));
    let invalid = AddressRange::new(window().start() + 1, window().end()).unwrap();
    assert!(matches!(make(invalid, 0), Err(Error::InvalidWindow)));
    let invalid = AddressRange::new(0x4000000000, 0x4000001000).unwrap();
    assert!(matches!(make(invalid, 0), Err(Error::InvalidWindow)));
    assert!(matches!(make(window(), 1), Err(Error::InvalidWindow)));
    assert!(matches!(make(window(), u64::MAX & !4095), Err(Error::Overflow)));
    let short = AddressRange::new(window().start(), window().start() + 4096).unwrap();
    let error = match make(short, ram[0].base)
    {
        Err(error) => error,
        Ok(_) => panic!("RAM escaped window")
    };
    assert!(matches!(error, Error::BankOutside { .. }));
    let message = error.to_string();
    assert!(message.contains("0x80000000+0x200000000") && message.contains("larger platform map"),
            "{message}");
    let map = make(window(), 0).unwrap();
    for address in [window().start() - 1, window().end()]
    {
        assert!(matches!(map.physical_address(VirtualAddress(address), 1),
                         Err(Error::OutsideWindow { .. })));
    }
    assert!(matches!(map.physical_address(VirtualAddress(window().start()), 0),
                     Err(Error::OutsideWindow { .. })));
    assert!(matches!(map.virtual_address(PhysicalAddress(ram[0].base), 0),
                     Err(Error::UnmappedRam { .. })));
    assert!(matches!(map.virtual_address(PhysicalAddress(u64::MAX), 2), Err(Error::Overflow)));
    assert!(matches!(map.physical_address(VirtualAddress(u64::MAX), 2), Err(Error::Overflow)));
    assert!(Error::InvalidWindow.to_string().contains("selected page geometry"));
    assert!(Error::Overflow.to_string().contains("overflow"));
}
