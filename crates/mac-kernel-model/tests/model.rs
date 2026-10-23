//! Pure model tests, not Toolbox conformance claims. No synthetic resident
//! services or test-only traps are introduced for these dormant structures.
use mac_kernel_model::{buddy::{BuddyPool, PageMetadata},
                       pages::*,
                       policy::*,
                       process::*,
                       zone::*};
use mac_object_table::domain::{CapacityPolicy, DomainKind, KindCode, OwnerIndex, ResourceKey};

struct WidePages;
impl Architecture for WidePages
{
    type PageTable = [u8; 65536];
    type TrapFrame = (u64, u64);

    // A backend type, not a common RV64 frame.
    fn geometry() -> PageGeometry
    {
        PageGeometry::new(65536).unwrap()
    }

    fn leaf_sizes() -> &'static [u64]
    {
        &[1 << 29, 1 << 16]
    }

    fn valid_virtual_range(range: AddressRange) -> bool
    {
        range.end() <= 1 << 48
    }
}
fn key(index: &mut OwnerIndex, code: &[u8; 4]) -> ResourceKey
{
    let kind = KindCode(*code);
    index.add_pool(kind, CapacityPolicy::GrowableUntilMemory).unwrap();
    index.grow(kind, 1).unwrap();
    index.allocate(kind).unwrap()
}
#[test]
fn layout_and_large_maps_do_not_assume_a_cpu_or_base_page_size()
{
    let range = |a, b| AddressRange::new(a, b).unwrap();
    let layout = UniformLayout::new(range(0, 1 << 32),
                                    range(1 << 32, 2 << 32),
                                    range(2 << 32, 3 << 32),
                                    range(3 << 32, 8 << 32),
                                    range(4 << 32, 8 << 32),
                                    1 << 16).unwrap();
    layout.validate_architecture::<WidePages>().unwrap();
    let mut script = String::new();
    layout.linker_symbols(&mut script).unwrap();
    assert!(script.contains("__APPLICATION_IMAGE_BASE = 0x10000;"));
    let mappings: Vec<_> = MappingPlan::new(PhysicalAddress(0),
                                            VirtualAddress(4 << 32),
                                            8 << 30,
                                            WidePages::geometry(),
                                            WidePages::leaf_sizes()).unwrap()
                                                                    .collect();
    assert_eq!(mappings.len(), 16);
    assert_eq!(mappings.iter().map(|m| m.bytes).sum::<u64>(), 8 << 30);
    // RV-sized leaves are test data, not geometry hardcoded into the model.
    let rv: Vec<_> = MappingPlan::new(PhysicalAddress(0),
                                      VirtualAddress(0),
                                      8 << 30,
                                      PageGeometry::new(4096).unwrap(),
                                      &[1 << 30, 1 << 21, 4096]).unwrap()
                                                                .collect();
    assert_eq!(rv.len(), 8);
    assert!(matches!(MappingPlan::new(PhysicalAddress(1),
                                      VirtualAddress(0),
                                      65536,
                                      WidePages::geometry(),
                                      WidePages::leaf_sizes()),
                     Err(ModelError::InvalidGeometry)));
    assert!(matches!(MappingPlan::new(PhysicalAddress(u64::MAX - 65535),
                                      VirtualAddress(0),
                                      65536,
                                      WidePages::geometry(),
                                      WidePages::leaf_sizes()),
                     Err(ModelError::Overflow)));
    assert!(format!("{}", ModelError::InvalidGeometry).contains("platform page geometry"));
    assert!(matches!(UniformLayout::new(range(0, 100),
                                        range(90, 200),
                                        range(200, 300),
                                        range(300, 500),
                                        range(320, 400),
                                        10),
                     Err(ModelError::InvalidLayout)));
}
#[test]
fn boot_reservations_are_one_checked_borrowed_account()
{
    let geometry = PageGeometry::new(4096).unwrap();
    let ram = [AddressRange::new(0, 65536).unwrap()];
    let reservations = [Reservation { range: AddressRange::new(0, 4096).unwrap(),
                                      kind:  ReservationKind::Firmware }];
    BootMemory { ram:          &ram,
                 reservations: &reservations }.validate(geometry)
                                              .unwrap();
    assert_eq!(BootMemory { ram:          &ram,
                            reservations: &[reservations[0], reservations[0]] }.validate(geometry),
               Err(ModelError::InvalidReservation));
    assert_eq!(BootMemory { ram:          &[],
                            reservations: &[] }.validate(geometry),
               Err(ModelError::InvalidReservation));
}
#[test]
fn process_zone_and_saved_state_cannot_mix_domain_incarnations()
{
    let mut owner = OwnerIndex::issue(DomainKind::Process).unwrap();
    let mut foreign = OwnerIndex::issue(DomainKind::Process).unwrap();
    let image = key(&mut owner, b"IMAG");
    let memory = key(&mut owner, b"MEMR");
    let legacy = key(&mut owner, b"LEGC");
    let instance = key(&mut owner, b"CTRL");
    let zone = key(&mut owner, b"ZONE");
    let other = key(&mut foreign, b"ZONE");
    assert!(matches!(ZoneBinding::new(instance, other), Err(ModelError::WrongDomain)));
    let binding = ZoneBinding::new(instance, zone).unwrap();
    assert_eq!(binding.owner(), owner.owner());
    let mut saved = SavedManagerState::empty(owner.owner());
    saved.register(instance).unwrap();
    assert_eq!(saved.register(instance), Err(ModelError::InvalidBinding));
    assert_eq!(saved.register(other), Err(ModelError::WrongDomain));
    let mut meta = [PageMetadata::unavailable(); 1];
    let mut heads = [None; 1];
    let pool=BuddyPool::new(PhysicalAddress(65536),WidePages::geometry(),&mut meta,&mut heads).unwrap();
    pool.add_free_range(AddressRange::new(65536, 131072).unwrap()).unwrap();
    let root = PageBox::<[u8; 65536]>::reserve(pool.allocate(0).unwrap()).unwrap();
    let space=AddressSpace::<WidePages>::reserve(owner.owner(),root,AsidTag{hardware_value:1,generation:core::num::NonZeroU64::new(1).unwrap()}).unwrap();
    assert_eq!(space.root_physical(), PhysicalAddress(65536));
    assert_eq!(space.planned_mappings(), 0);
    let mut process = ProcessRecord::new(owner.owner(), image, memory, legacy, space).unwrap();
    let thread = key(&mut owner, b"THRD");
    let profile = ConfiguredRange::new(65536, 65536, 131072).unwrap();
    let stack=StackReservation::new(owner.owner(),AddressRange::new(0,65536).unwrap(),profile).unwrap();
    let vector = VectorStorage::lazy(core::alloc::Layout::from_size_align(2048, 64).unwrap());
    process.add_thread(ThreadRecord::<WidePages>::new(thread,
                                                      stack,
                                                      SavedFrame::new(owner.owner(), (1, 2)),
                                                      vector).unwrap())
           .unwrap();
    assert_eq!(process.threads().len(), 1);
    assert!(!process.may_hold_vector_state());
    let frame = SavedFrame::<WidePages>::new(foreign.owner(), (0, 0));
    let stack=StackReservation::new(owner.owner(),AddressRange::new(0,65536).unwrap(),profile).unwrap();
    assert!(matches!(ThreadRecord::new(thread,
                                       stack,
                                       frame,
                                       VectorStorage::lazy(core::alloc::Layout::new::<u64>())),
                     Err(ModelError::WrongDomain)));
    drop(process);
    assert_eq!(pool.free_pages(), 1);
    let heap = ConfiguredRange::new(100, 200, 300).unwrap();
    assert!(matches!(SystemReserve::new(heap, 201, 100), Err(ModelError::InvalidReserve)));
    let reserve = SystemReserve::new(heap, 100, 50).unwrap();
    let mut registry = DomainRegistry::new(reserve);
    let identity = owner.owner();
    registry.insert(DomainRecord::new(owner, profile, profile)).unwrap();
    let record = registry.domains().next().unwrap();
    assert_eq!(record.account().owner(), identity);
    assert_eq!(record.ledger().owner(), identity);
    assert_eq!(record.account().kernel().committed(), 0);
    assert_eq!(record.state(), DomainState::Constructing);
}

#[test]
fn fragmented_pool_matches_an_independent_page_occupancy_oracle()
{
    // Different unaligned bases and non-power-of-two pool sizes catch coalesce
    // errors that a single naturally aligned arena hides.
    for base_page in [0, 1, 3, 7]
    {
        let pages = 127;
        let page = 4096;
        let mut metadata = vec![PageMetadata::unavailable(); pages];
        let mut heads = vec![None; BuddyPool::required_orders(pages)];
        let pool = BuddyPool::new(PhysicalAddress(base_page * page),
                                  PageGeometry::new(page).unwrap(),
                                  &mut metadata,
                                  &mut heads).unwrap();
        pool.add_free_range(AddressRange::new(base_page*page,(base_page+pages as u64)*page).unwrap()).unwrap();
        let mut blocks = Vec::new();
        let mut random = base_page + 1;
        for _ in 0..2000
        {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            if random & 3 == 0 && !blocks.is_empty()
            {
                blocks.swap_remove((random as usize >> 16) % blocks.len());
            }
            else if let Ok(block) = pool.allocate(((random >> 32) % 6) as u32)
            {
                blocks.push(block);
            }
            let mut occupied = vec![false; pages];
            let mut used = 0;
            for block in &blocks
            {
                let start = ((block.address().0 - base_page * page) / page) as usize;
                let count = (block.bytes() / page) as usize;
                assert_eq!(block.address().0 % block.bytes(), 0);
                assert!(start + count <= pages);
                for bit in &mut occupied[start..start + count]
                {
                    assert!(!*bit, "overlapping allocations");
                    *bit = true;
                }
                used += count;
            }
            assert_eq!(pool.free_pages() + used, pages);
        }
        drop(blocks);
        assert_eq!(pool.free_pages(), pages);
        // Every free page can be reacquired after arbitrary fragmentation.
        let blocks: Vec<_> = (0..pages).map(|_| pool.allocate(0).unwrap()).collect();
        assert_eq!(pool.free_pages(), 0);
        drop(blocks);
    }
}

#[test]
fn page_leases_are_safe_to_allocate_and_release_from_multiple_threads()
{
    let mut metadata = vec![PageMetadata::unavailable(); 256];
    let mut heads = vec![None; 9];
    let pool = BuddyPool::new(PhysicalAddress(0),
                              PageGeometry::new(4096).unwrap(),
                              &mut metadata,
                              &mut heads).unwrap();
    pool.add_free_range(AddressRange::new(0, 256 * 4096).unwrap()).unwrap();
    std::thread::scope(|scope| {
        for _ in 0..4
        {
            let pool = &pool;
            scope.spawn(move || {
                     for _ in 0..500
                     {
                         let a = pool.allocate(0).unwrap();
                         let b = pool.allocate(1).unwrap();
                         assert_ne!(a.address(), b.address());
                         drop(a);
                         drop(b);
                     }
                 });
        }
    });
    assert_eq!(pool.free_pages(), 256);
    assert_eq!(pool.allocate(8).unwrap().bytes(), 256 * 4096);
}
