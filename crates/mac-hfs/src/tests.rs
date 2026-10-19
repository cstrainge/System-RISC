use alloc::{vec, vec::Vec};

use super::*;

struct Disk(Vec<u8>);
impl BlockDevice for Disk
{
    fn sector_count(&self) -> u64
    {
        self.0.len() as u64 / 512
    }

    fn read_sector(&mut self, sector: u64, output: &mut [u8; 512]) -> Result<()>
    {
        output.copy_from_slice(bytes(&self.0, sector as usize * 512, 512)?);
        Ok(())
    }
}
fn put16(b: &mut [u8], p: usize, v: u16)
{
    b[p..p + 2].copy_from_slice(&v.to_be_bytes());
}
fn put32(b: &mut [u8], p: usize, v: u32)
{
    b[p..p + 4].copy_from_slice(&v.to_be_bytes());
}
fn node(records: &[Vec<u8>], previous: u32, next: u32) -> Vec<u8>
{
    let mut b = vec![0; 512];
    put32(&mut b, 0, next);
    put32(&mut b, 4, previous);
    b[8] = 0xff;
    b[9] = 1;
    put16(&mut b, 10, records.len() as u16);
    let mut start = 14;
    for (i, record) in records.iter().enumerate()
    {
        put16(&mut b, 512 - (i + 1) * 2, start as u16);
        b[start..start + record.len()].copy_from_slice(record);
        start += record.len();
    }
    put16(&mut b, 512 - (records.len() + 1) * 2, start as u16);
    b
}
fn tree_header(first: u32, last: u32, total: u32) -> Vec<u8>
{
    let mut b = vec![0; 512];
    b[8] = 1;
    put32(&mut b, 24, first);
    put32(&mut b, 28, last);
    put16(&mut b, 32, 512);
    put32(&mut b, 36, total);
    b
}
fn catalog(parent: u32, name: &[u8], data: &[u8]) -> Vec<u8>
{
    let length = (6 + name.len()) | 1;
    let mut out = vec![0; length + 1];
    out[0] = length as u8;
    put32(&mut out, 2, parent);
    out[6] = name.len() as u8;
    out[7..7 + name.len()].copy_from_slice(name);
    out.extend_from_slice(data);
    out
}
fn fixture() -> Disk
{
    let mut b = vec![0; 64 * 512];
    let m = &mut b[1024..1536];
    m[..2].copy_from_slice(b"BD");
    put16(m, 18, 58);
    put32(m, 20, 512);
    put16(m, 28, 4);
    m[36] = 4;
    m[37..41].copy_from_slice(b"Test");
    put32(m, 92, 16);
    put32(m, 130, 1024);
    put16(m, 134, 4);
    put16(m, 136, 2);
    put32(m, 146, 2048);
    put16(m, 150, 0);
    put16(m, 152, 4);
    b[4 * 512..5 * 512].copy_from_slice(&tree_header(1, 2, 4));
    let mut directory = vec![0; 70];
    directory[0] = 1;
    put32(&mut directory, 6, 16);
    b[5 * 512..6 * 512].copy_from_slice(&node(&[catalog(2, b"Renamed Folder", &directory)], 0, 2));
    let mut file = vec![0; 102];
    file[0] = 2;
    file[4..12].copy_from_slice(b"zsysMACS");
    put32(&mut file, 20, 19);
    put32(&mut file, 36, 1700);
    for (i, start) in [6, 8, 10].into_iter().enumerate()
    {
        put16(&mut file, 86 + i * 4, start);
        put16(&mut file, 88 + i * 4, 1);
    }
    b[6 * 512..7 * 512].copy_from_slice(&node(&[catalog(16, b"System", &file)], 1, 0));
    b[8 * 512..9 * 512].copy_from_slice(&tree_header(1, 1, 2));
    let mut overflow = vec![0; 20];
    overflow[0] = 7;
    overflow[1] = 0xff;
    put32(&mut overflow, 2, 19);
    put16(&mut overflow, 6, 3);
    put16(&mut overflow, 8, 12);
    put16(&mut overflow, 10, 1);
    b[9 * 512..10 * 512].copy_from_slice(&node(&[overflow], 0, 0));
    for (index, sector) in [10, 12, 14, 16].into_iter().enumerate()
    {
        for j in 0..512
        {
            b[sector * 512 + j] = ((index * 512 + j) % 251) as u8;
        }
    }
    Disk(b)
}

#[test]
fn blessing_by_id_leaf_chain_and_fragmented_overflow_fork()
{
    let mut disk = fixture();
    let partition = partitions(&mut disk).unwrap()[0];
    let mut volume = Volume::mount(&mut disk, partition).unwrap();
    let folder = volume.blessed_folder().unwrap();
    assert_eq!(folder.name, b"Renamed Folder");
    let file = volume.find_file(folder.id, b"sYsTeM").unwrap();
    assert_eq!(file.file_type, *b"zsys");
    let bytes = volume.read_resources(&file, 2048).unwrap();
    assert_eq!(bytes, (0..1700).map(|i| (i % 251) as u8).collect::<Vec<_>>());
    assert!(volume.read_resources(&file, 100).is_err());
    assert!(volume.find_file(2, b"System").is_err());
}

#[test]
fn partition_map_and_out_of_range_partition()
{
    let original = fixture();
    let mut disk = Disk(vec![0; 72 * 512]);
    disk.0[8 * 512..].copy_from_slice(&original.0);
    let entry = &mut disk.0[512..1024];
    entry[..2].copy_from_slice(b"PM");
    put32(entry, 4, 1);
    put32(entry, 8, 8);
    put32(entry, 12, 64);
    entry[48..57].copy_from_slice(b"Apple_HFS");
    let p = partitions(&mut disk).unwrap()[0];
    assert_eq!(p.start, 8);
    assert_eq!(Volume::mount(&mut disk, p).unwrap().blessed_folder().unwrap().id, 16);
    put32(&mut disk.0, 512 + 12, 65);
    assert!(partitions(&mut disk).is_err());
}

#[test]
fn rejects_missing_blessing_dangling_cnid_and_corrupt_geometry()
{
    for id in [0, 99]
    {
        let mut disk = fixture();
        put32(&mut disk.0, 1024 + 92, id);
        let mut volume = Volume::mount(&mut disk,
                                       Partition { start:   0,
                                                   sectors: 64 }).unwrap();
        assert!(volume.blessed_folder().is_err());
    }
    let mut disk = fixture();
    put32(&mut disk.0, 1024 + 20, u32::MAX);
    assert!(Volume::mount(&mut disk,
                          Partition { start:   0,
                                      sectors: 64 }).is_err());
}

#[test]
fn rejects_cycles_record_offsets_and_invalid_extents()
{
    let mut disk = fixture();
    put32(&mut disk.0, 6 * 512, 1);
    let mut volume = Volume::mount(&mut disk,
                                   Partition { start:   0,
                                               sectors: 64 }).unwrap();
    assert!(volume.find_file(16, b"Missing").is_err());
    let mut disk = fixture();
    put16(&mut disk.0, 5 * 512 + 510, 511);
    let mut volume = Volume::mount(&mut disk,
                                   Partition { start:   0,
                                               sectors: 64 }).unwrap();
    assert!(volume.blessed_folder().is_err());
    let mut disk = fixture();
    put16(&mut disk.0, 1024 + 150, 60);
    assert!(Volume::mount(&mut disk,
                          Partition { start:   0,
                                      sectors: 64 }).is_err());
}

#[test]
fn rejects_truncated_overflow_record()
{
    let mut disk = fixture();
    disk.0[9 * 512..10 * 512].copy_from_slice(&node(&[vec![7]], 0, 0));
    let mut volume = Volume::mount(&mut disk,
                                   Partition { start:   0,
                                               sectors: 64 }).unwrap();
    assert!(volume.find_file(16, b"System").is_err());
}

#[test]
fn catalog_metadata_both_forks_and_empty_eof_reads()
{
    let mut disk = fixture();
    let p = partitions(&mut disk).unwrap()[0];
    let mut volume = Volume::mount(&mut disk, p).unwrap();
    let folder = volume.entry(16).unwrap().unwrap();
    assert!(folder.directory);
    assert_eq!(folder.parent, 2);
    assert_eq!(volume.child(16, 0).unwrap().unwrap().name, b"System");
    assert!(volume.child(16, 1).unwrap().is_none());
    assert!(volume.entry(999).unwrap().is_none());
    let file = volume.lookup(16, b"sYsTeM").unwrap().unwrap();
    assert_eq!(&file.finder_info[..8], b"zsysMACS");
    let mut empty = file.data.unwrap();
    let mut output = [0xa5; 40];
    assert_eq!(volume.read(&mut empty, 0, &mut output).unwrap(), 0);
    assert_eq!(output, [0xa5; 40]);
    let mut resources = file.resources.unwrap();
    assert_eq!(volume.read(&mut resources, 1690, &mut output).unwrap(), 10);
    assert_eq!(&output[..10], (1690..1700).map(|n| (n % 251) as u8).collect::<Vec<_>>().as_slice());
    assert_eq!(output[10], 0xa5);
    assert!(volume.read(&mut resources, 1701, &mut output).is_err());
}
#[test]
fn classic_catalog_folding_uses_original_not_unicode_equality()
{
    assert!(names_equal(b"CAF\x83", b"caf\x8e"));
    assert!(names_equal(b"A B", b"a\xcaB")); // original HFS space / nonbreaking-space equivalence
    assert!(!names_equal(b"cafe", b"caf\x8e"));
    assert!(!names_equal(b"\xe7", b"\x87")); // later uppercase A-acute wasn't folded by classic comparator
}
#[test]
fn apm_logical_data_offset_is_bounded_and_mounted()
{
    let original = fixture();
    let mut disk = Disk(vec![0; 80 * 512]);
    disk.0[10 * 512..74 * 512].copy_from_slice(&original.0);
    let p = &mut disk.0[512..1024];
    p[..2].copy_from_slice(b"PM");
    put32(p, 4, 1);
    put32(p, 8, 8);
    put32(p, 12, 66);
    p[48..57].copy_from_slice(b"Apple_HFS");
    put32(p, 80, 2);
    put32(p, 84, 64);
    let partition = partitions(&mut disk).unwrap()[0];
    assert_eq!(partition,
               Partition { start:   10,
                           sectors: 64 });
    assert_eq!(Volume::mount(&mut disk, partition).unwrap().name, b"Test");
    put32(&mut disk.0, 512 + 84, 65);
    assert!(partitions(&mut disk).is_err());
}
