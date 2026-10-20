//! Actual allocated fork I/O beyond both legacy length boundaries.
use mac_hfs_b::*;
#[path = "support/large.rs"]
mod large;
#[test]
#[ignore = "streams over 4 GiB of actual fork writes; run with --release --ignored"]
fn allocated_fork_io_crosses_two_and_four_gib_and_remounts()
{
    let (mut disk, id) = large::allocated_fork();
    let partition = Partition { start:   0,
                                sectors: disk.sectors };
    let budget = Budget::default();
    assert!(disk.written > 1u64 << 32);
    let mut v = Volume::mount(&mut disk, partition, budget).unwrap();
    for boundary in [1u64 << 31, 1u64 << 32]
    {
        let mut out = [0; 16];
        v.read_fork(id, ForkKind::Data, boundary - 8, &mut out).unwrap();
        assert_eq!(&out, b"boundary-crossed");
    }
    assert_eq!(v.entry(id).unwrap().data.blocks, ((1u64 << 32) + 4096) / 4096);
    v.check().unwrap();
}
