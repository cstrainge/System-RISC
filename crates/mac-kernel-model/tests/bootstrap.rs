use core::{alloc::Layout, mem::MaybeUninit};

use mac_abi::{BootMemoryRange as Range, boot::Memory};
use mac_kernel_model::{bootstrap::{BankPlan, Error},
                       buddy::{BuddyPool, PageMetadata},
                       pages::{PageGeometry, PhysicalAddress},
                       policy::AddressRange};
use mac_object_table::shared::InvocationGate;
struct Gate;
impl InvocationGate for Gate
{
    fn run<R>(f: impl FnOnce() -> R) -> R
    {
        f()
    }
}
fn range(base: u64, length: u64) -> Range
{
    Range { base,
            length }
}
#[test]
fn sparse_banks_share_the_rom_account_without_metadata_for_holes()
{
    let banks = [range(0x80000000, 8 << 30), range(0x100000000000, 16 << 20)];
    // Deliberately unsorted and split across banks.
    let reserved = [range(banks[1].base, 4096),
                    range(banks[0].base + 0x10000, 4096),
                    range(banks[0].base, 4096),
                    range(banks[0].base + banks[0].length - 4096, 4096)];
    let memory = Memory { ram:          &banks,
                          reservations: &reserved }.checked(4096, banks[0])
                                                   .unwrap();
    let a = BankPlan::new(memory, 0, Layout::new::<[usize; 8]>()).unwrap().unwrap();
    let b = BankPlan::new(memory, 1, Layout::new::<[usize; 8]>()).unwrap().unwrap();
    assert_eq!(a.layout().pages(), 2_097_152);
    assert_eq!(b.layout().pages(), 4096);
    assert_eq!(a.layout().orders(), 22);
    assert_eq!(b.layout().orders(), 13);
    assert_eq!(a.storage().base, banks[0].base + 0x11000);
    assert_eq!(b.storage().base, banks[1].base + 4096);
    for plan in [&a, &b]
    {
        let free: Vec<_> = plan.free_ranges().collect();
        for item in &free
        {
            assert!(item.length > 0 && item.base % 4096 == 0 && item.length % 4096 == 0);
            for reservation in reserved.iter().chain(core::iter::once(&plan.storage()))
            {
                assert!(!(item.base < reservation.base + reservation.length
                          && reservation.base < item.base + item.length));
            }
        }
        let excluded: u64 = reserved.iter()
                                    .filter(|r| {
                                        r.base >= plan.bank().base
                                        && r.base < plan.bank().base + plan.bank().length
                                    })
                                    .map(|r| r.length)
                                    .sum();
        assert_eq!(free.iter().map(|r| r.length).sum::<u64>() + excluded + plan.storage().length,
                   plan.bank().length);
        let layout = plan.layout();
        assert!(layout.metadata_offset() >= core::mem::size_of::<[usize; 8]>());
        assert!(layout.heads_offset()
                >= layout.metadata_offset() + layout.pages() * size_of::<PageMetadata>());
        assert!(layout.bytes() as usize
                >= layout.heads_offset() + layout.orders() * size_of::<Option<usize>>());
    }
    // Iterating yields extents, not millions of individual page records.
    assert_eq!(a.free_ranges().count(), 2);
    assert_eq!(b.free_ranges().count(), 1);
}
#[test]
fn fragmented_metadata_refusal_and_fully_reserved_banks_are_honest()
{
    let banks = [range(0, 2 << 20)];
    // 4KiB holes have plenty of total bytes but no span for 512 page records.
    let reserved: Vec<_> = (0..256).map(|i| range(i * 8192, 4096)).collect();
    let memory = Memory { ram:          &banks,
                          reservations: &reserved }.checked(4096, banks[0])
                                                   .unwrap();
    let error = match BankPlan::new(memory, 0, Layout::new::<usize>())
    {
        Err(error) => error,
        Ok(_) => panic!("metadata was admitted into a fragmented span")
    };
    assert!(matches!(error,
                     Error::NoStorage { largest_free_bytes: 4096,
                                        .. }));
    let message = error.to_string();
    assert!(message.contains("needs")
            && message.contains("contiguous")
            && message.contains("4096"),
            "{message}");
    assert!(matches!(BankPlan::new(memory, 1, Layout::new::<usize>()),
                     Err(Error::Bank { index: 1 })));
    assert!(Error::Bank { index: 1 }.to_string().contains("bank 1 does not exist"));
    let full = Memory { ram:          &banks,
                        reservations: &banks }.checked(4096, banks[0])
                                              .unwrap();
    assert!(BankPlan::new(full, 0, Layout::new::<usize>()).unwrap().is_none());
    let oversized = Layout::from_size_align(4096, 8192).unwrap();
    assert!(matches!(BankPlan::new(memory, 0, oversized), Err(Error::Layout { .. })));
    assert!(Error::Layout { bank: banks[0] }.to_string().contains("metadata layout"));
}
#[test]
fn initialized_storage_excludes_itself_from_every_buddy_lease()
{
    let banks = [range(0x90000000, 64 * 4096)];
    let reserved = [range(banks[0].base, 2 * 4096), range(banks[0].base + 20 * 4096, 4096)];
    let memory = Memory { ram:          &banks,
                          reservations: &reserved }.checked(4096, banks[0])
                                                   .unwrap();
    let plan = BankPlan::new(memory, 0, Layout::new::<usize>()).unwrap().unwrap();
    // These are independent host backing stores for the plan's described RAM.
    // The native binding later supplies exclusive mapped views of those bytes.
    let mut metadata = vec![MaybeUninit::uninit(); plan.layout().pages()];
    let mut heads = vec![MaybeUninit::uninit(); plan.layout().orders()];
    let pool = BuddyPool::<Gate>::initialize(PhysicalAddress(plan.bank().base),
                                             PageGeometry::new(4096).unwrap(),
                                             &mut metadata,
                                             &mut heads).unwrap();
    let free: u64 = plan.free_ranges().map(|r| r.length / 4096).sum();
    for span in plan.free_ranges()
    {
        pool.add_free_range(AddressRange::new(span.base, span.base + span.length).unwrap())
            .unwrap();
    }
    assert_eq!(pool.free_pages() as u64, free);
    let mut leases = Vec::new();
    while let Ok(page) = pool.allocate(0)
    {
        let address = page.address().0;
        for r in reserved.iter().chain(core::iter::once(&plan.storage()))
        {
            assert!(address < r.base || address >= r.base + r.length);
        }
        leases.push(page);
    }
    assert_eq!(leases.len() as u64, free);
    assert_eq!(pool.free_pages(), 0);
    drop(leases);
    assert_eq!(pool.free_pages() as u64, free);
    // A malformed initialization refuses before writing caller's storage.
    let mut untouched = [MaybeUninit::new(Some(123usize)); 1];
    assert!(BuddyPool::<Gate>::initialize(PhysicalAddress(1),
                                          PageGeometry::new(4096).unwrap(),
                                          &mut metadata,
                                          &mut untouched).is_err());
    // SAFETY: every element started initialized and refusal wrote nothing.
    assert_eq!(unsafe { untouched[0].assume_init() }, Some(123));
}
