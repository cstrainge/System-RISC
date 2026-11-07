//! A transport set with occupied and unoccupied entries: what the bus reports
//! and, just as importantly, which addresses it touches to find out.

use core::cell::{Cell, RefCell};

use mac_device_interface::*;

/// Three transports as a machine may place them: unevenly, with the positions
/// between them belonging to nobody.
const TRANSPORTS: [(u64, u64); 3] = [(0x1000, 0x200), (0x3000, 0x200), (0x5000, 0x200)];

/// A window's identity words, keyed by the window's own base. The other
/// transports hold no device, which is what an empty transport states: zeros.
fn words(base: u64) -> [u32; 4]
{
    match base
    {
        0x1000 => [VIRTIO_MAGIC, VIRTIO_VERSION, 16, 0x1af4],
        _ => [0; 4]
    }
}

#[test]
fn an_unoccupied_transport_is_absent_and_only_its_own_window_is_read()
{
    let touched = RefCell::new(Vec::new());
    let described: Vec<Option<(u32, u32)>> = TRANSPORTS.iter()
                                                       .map(|(base, bytes)| {
                                                           virtio_identity(*bytes, |offset| {
                                                               touched.borrow_mut()
                                                                      .push((*base, offset));
                                                               words(*base)[offset / 4]
                                                           })
                                                       })
                                                       .collect();
    // The occupied entry is the device the window states, identified by its own
    // words rather than by its position in the set.
    assert_eq!(described[0], Some((16, 0x1af4)));
    // This is the assertion that makes an empty position a lookup result. An
    // enumerator that computed windows from an ordinal would report a device
    // here, or read an address the machine never stated to find out.
    assert_eq!(described[1], None);
    assert_eq!(described[2], None);
    let touched = touched.into_inner();
    let mut bases: Vec<u64> = touched.iter().map(|(base, _)| *base).collect();
    bases.sort_unstable();
    bases.dedup();
    assert_eq!(bases, [0x1000, 0x3000, 0x5000]);
    assert!(touched.iter().all(|(_, offset)| *offset < VIRTIO_IDENTITY_BYTES as usize));
}

#[test]
fn a_window_that_cannot_hold_an_identity_is_never_read()
{
    let read = Cell::new(false);
    assert_eq!(virtio_identity(VIRTIO_IDENTITY_BYTES - 4, |_| {
                   read.set(true);
                   0
               }),
               None);
    assert!(!read.get());
}

#[test]
fn a_window_that_states_no_device_type_is_absent()
{
    // A transport with the right magic and version but no device behind it is
    // the shape a machine uses to describe a position it has not populated.
    assert_eq!(virtio_identity(VIRTIO_IDENTITY_BYTES, |offset| {
                   [VIRTIO_MAGIC, VIRTIO_VERSION, 0, 0x1af4][offset / 4]
               }),
               None);
}
