use std::sync::{Arc,
                atomic::{AtomicUsize, Ordering}};

use mac_driver_alloc::Reservation;
#[repr(align(4096))]
struct Owned(Arc<AtomicUsize>);
impl Drop for Owned
{
    fn drop(&mut self)
    {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}
#[test]
fn reserved_storage_transfers_aligned_ownership_and_destroys_value_exactly_once()
{
    let drops = Arc::new(AtomicUsize::new(0));
    let reservation = Reservation::<Owned>::try_new().unwrap();
    let value = reservation.write(Owned(drops.clone()));
    assert_eq!((&*value as *const Owned).addr() % 4096, 0);
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    drop(value);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    // An uninitialized reservation never runs a destructor for a missing value.
    drop(Reservation::<Owned>::try_new().unwrap());
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}
struct Empty;
static ZST_DROPS: AtomicUsize = AtomicUsize::new(0);
impl Drop for Empty
{
    fn drop(&mut self)
    {
        ZST_DROPS.fetch_add(1, Ordering::Relaxed);
    }
}
#[test]
fn zero_sized_reservations_use_box_ownership_without_allocating_or_double_drop()
{
    let reserved = Reservation::<Empty>::try_new().unwrap();
    let value = reserved.write(Empty);
    assert_eq!(ZST_DROPS.load(Ordering::Relaxed), 0);
    drop(value);
    assert_eq!(ZST_DROPS.load(Ordering::Relaxed), 1);
    drop(Reservation::<Empty>::try_new().unwrap());
    assert_eq!(ZST_DROPS.load(Ordering::Relaxed), 1);
}
