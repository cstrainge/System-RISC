use mac_hfs_b::{layout::{BLOCK_SIZE, ROOT_ID},
                time::Timestamp,
                *};

#[derive(Clone)]
struct Disk
{
    bytes:           Vec<u8>,
    read_only:       bool,
    writes:          usize,
    fail_at:         Option<usize>,
    flushes:         usize,
    fail_flush:      Option<usize>,
    fail_persistent: bool
}
impl Disk
{
    fn new() -> Self
    {
        Self { bytes:           vec![0; 16 * 1024 * 1024],
               read_only:       false,
               writes:          0,
               fail_at:         None,
               flushes:         0,
               fail_flush:      None,
               fail_persistent: false }
    }

    fn partition(&self) -> Partition
    {
        Partition { start:   0,
                    sectors: self.sector_count() }
    }
}
impl BlockDevice for Disk
{
    fn sector_count(&self) -> u64
    {
        (self.bytes.len() / 512) as u64
    }

    fn read_sector(&mut self, sector: u64, out: &mut [u8; 512]) -> mac_block::Result<()>
    {
        let start = usize::try_from(sector).ok()
                                           .and_then(|n| n.checked_mul(512))
                                           .ok_or(mac_block::Error("bounds"))?;
        out.copy_from_slice(self.bytes
                                .get(start..start + 512)
                                .ok_or(mac_block::Error("bounds"))?);
        Ok(())
    }
}
impl WritableBlockDevice for Disk
{
    fn is_read_only(&self) -> bool
    {
        self.read_only
    }

    fn write_sector(&mut self, sector: u64, input: &[u8; 512]) -> mac_block::Result<()>
    {
        self.writes += 1;
        if self.read_only
           || self.fail_at
                  .is_some_and(|n| self.writes == n || (self.fail_persistent && self.writes >= n))
        {
            return Err(mac_block::Error("write injected"));
        }
        let start = sector as usize * 512;
        self.bytes
            .get_mut(start..start + 512)
            .ok_or(mac_block::Error("bounds"))?
            .copy_from_slice(input);
        Ok(())
    }

    fn flush(&mut self) -> mac_block::Result<()>
    {
        self.flushes += 1;
        if self.fail_flush == Some(self.flushes)
        {
            Err(mac_block::Error("flush injected"))
        }
        else
        {
            Ok(())
        }
    }
}
fn formatted() -> Disk
{
    let mut d = Disk::new();
    let p = d.partition();
    format(&mut d,
           p,
           FormatOptions { name:    "System RISC",
                           created: Timestamp(3_875_000_000_123_456),
                           uuid:    [7; 16] },
           Budget::default()).unwrap();
    d
}
fn mount(d: &mut Disk) -> Volume<'_, Disk>
{
    let p = d.partition();
    Volume::mount(d, p, Budget::default()).unwrap()
}
#[test]
fn format_forks_precise_metadata_and_remount()
{
    let mut d = formatted();
    let id;
    {
        let mut v = mount(&mut d);
        assert_eq!(v.check().unwrap().directories, 1);
        let system = v.create(ROOT_ID, "System Folder", true, Timestamp(0)).unwrap();
        v.bless(system.id, Timestamp(0)).unwrap();
        let e = v.create(system.id, "cafe\u{301}", false, Timestamp(123_456)).unwrap();
        id = e.id;
        assert_eq!(e.name, "café");
        assert_eq!(v.lookup(system.id, "CAFÉ").unwrap().id, id);
        assert_eq!(v.create(system.id, "CAFÉ", false, Timestamp(0)).unwrap_err(), Error::Exists);
        v.write_fork(id, ForkKind::Data, 0, b"data", Timestamp(2_000_001)).unwrap();
        v.write_fork(id, ForkKind::Resource, 0, b"resource", Timestamp(2_000_002)).unwrap();
        v.write_fork(id, ForkKind::Data, 8193, b"tail", Timestamp(2_000_003)).unwrap();
        let mut bytes = vec![1; 8197];
        assert_eq!(v.read_fork(id, ForkKind::Data, 0, &mut bytes).unwrap(), bytes.len());
        assert_eq!(&bytes[..4], b"data");
        assert!(bytes[4..8193].iter().all(|&b| b == 0));
        assert_eq!(&bytes[8193..], b"tail");
        v.set_comment(id, "résumé 😀", Timestamp(9)).unwrap();
        v.rename(id, system.id, "CAFÉ", Timestamp(10)).unwrap();
        assert_eq!(v.entry(id).unwrap().name, "CAFÉ");
        assert_eq!(v.check().unwrap().files, 1);
    }
    let mut v = mount(&mut d);
    let e = v.entry(id).unwrap();
    assert_eq!(e.created, Timestamp(123_456));
    assert_eq!(e.modified, Timestamp(10));
    assert_eq!(v.comment(id).unwrap(), "résumé 😀");
    assert_eq!(v.blessed_folder().unwrap().name, "System Folder");
    let mut resource = [0; 8];
    v.read_fork(id, ForkKind::Resource, 0, &mut resource).unwrap();
    assert_eq!(&resource, b"resource");
    v.set_fork_len(id, ForkKind::Data, 2, Timestamp(11)).unwrap();
    v.set_fork_len(id, ForkKind::Data, 5000, Timestamp(12)).unwrap();
    let mut bytes = vec![1; 5000];
    v.read_fork(id, ForkKind::Data, 0, &mut bytes).unwrap();
    assert_eq!(&bytes[..2], b"da");
    assert!(bytes[2..].iter().all(|&b| b == 0));
    v.remove(id, Timestamp(13)).unwrap();
    assert_eq!(v.entry(id).unwrap_err(), Error::NotFound);
    assert_eq!(v.check().unwrap().files, 0);
}
#[test]
fn unicode_storage_collision_and_name_boundaries()
{
    let mut d = formatted();
    let mut v = mount(&mut d);
    for (a, b) in [("Straße", "STRASSE"),
                   ("ﬁ", "fi"),
                   ("I", "i"),
                   ("cafe\u{301}", "CAFÉ"),
                   ("Å\u{323}", "A\u{323}\u{30a}")]
    {
        let e = v.create(2, a, false, Timestamp(0)).unwrap();
        assert_eq!(v.lookup(2, b).unwrap().id, e.id);
        assert_eq!(v.create(2, b, false, Timestamp(0)).unwrap_err(), Error::Exists);
    }
    v.create(2, "ı", false, Timestamp(0)).unwrap();
    v.create(2, "①", false, Timestamp(0)).unwrap();
    v.create(2, "1", false, Timestamp(0)).unwrap();
    for name in ["a".repeat(255), format!("{}abc", "😀".repeat(63)), "e\u{301}".repeat(127)]
    {
        assert!(name.len() > 255 || name.len() == 255);
        v.create(2, &name, false, Timestamp(0)).unwrap();
    }
    let count = v.entry(2).unwrap().children;
    for name in ["a".repeat(256), "😀".repeat(64), "é".repeat(128)]
    {
        assert_eq!(v.create(2, &name, false, Timestamp(0)).unwrap_err(), Error::Invalid);
    }
    assert_eq!(v.entry(2).unwrap().children, count);
    v.check().unwrap();
}
#[test]
fn paged_tree_growth_rename_delete_and_fragmented_forks()
{
    let mut d = formatted();
    let mut v = mount(&mut d);
    let free = v.header().free;
    let mut ids = Vec::new();
    for i in (0..120).rev()
    {
        ids.push(v.create(2, &format!("File {i:04} {}", "界".repeat(60)), false, Timestamp(i))
                  .unwrap()
                  .id);
    }
    v.check().unwrap();
    let id = ids[0];
    let blocker = ids[1];
    for i in 0..25u64
    {
        v.write_fork(id,
                     ForkKind::Resource,
                     i * BLOCK_SIZE as u64,
                     &vec![i as u8; BLOCK_SIZE],
                     Timestamp(i))
         .unwrap();
        v.write_fork(blocker,
                     ForkKind::Data,
                     i * BLOCK_SIZE as u64,
                     &vec![255; BLOCK_SIZE],
                     Timestamp(i))
         .unwrap();
    }
    assert!(v.entry(id).unwrap().resources.blocks > 8);
    let mut output = vec![0; BLOCK_SIZE];
    for i in 0..25u64
    {
        v.read_fork(id, ForkKind::Resource, i * BLOCK_SIZE as u64, &mut output).unwrap();
        assert!(output.iter().all(|&b| b == i as u8));
    }
    v.check().unwrap();
    for id in ids
    {
        v.remove(id, Timestamp(0)).unwrap();
    }
    assert_eq!(v.check().unwrap().objects, 1);
    assert_eq!(v.header().free, free);
}
#[test]
fn reported_write_and_flush_failures_preserve_old_state()
{
    let mut original = formatted();
    let id = mount(&mut original).create(2, "stable", false, Timestamp(1)).unwrap().id;
    mount(&mut original).write_fork(id, ForkKind::Data, 0, b"original", Timestamp(2)).unwrap();
    // Exercise every sector write through payload, dirty headers, metadata and
    // clean publication. The single injected failure permits rollback I/O.
    for fail in 1..90
    {
        let mut d = original.clone();
        d.fail_at = Some(d.writes + fail);
        let result = mount(&mut d).write_fork(id, ForkKind::Data, 0, b"changed!", Timestamp(3));
        let mut v = mount(&mut d);
        v.check().unwrap();
        let mut out = [0; 8];
        v.read_fork(id, ForkKind::Data, 0, &mut out).unwrap();
        if result.is_err()
        {
            assert_eq!(&out, b"original", "write {fail}");
        }
        else
        {
            assert_eq!(&out, b"changed!");
        }
    }
    for fail in 1..7
    {
        let mut d = original.clone();
        d.fail_flush = Some(d.flushes + fail);
        let result = mount(&mut d).write_fork(id, ForkKind::Data, 0, b"changed!", Timestamp(3));
        let mut v = mount(&mut d);
        v.check().unwrap();
        let mut out = [0; 8];
        v.read_fork(id, ForkKind::Data, 0, &mut out).unwrap();
        assert_eq!(&out, if result.is_err() { b"original" } else { b"changed!" });
    }
    let mut d = original.clone();
    d.read_only = true;
    assert_eq!(mount(&mut d).write_fork(id, ForkKind::Data, 0, b"no", Timestamp(0)).unwrap_err(),
               Error::ReadOnly);
}
#[test]
fn checked_epoch_and_fractional_seconds()
{
    assert_eq!(Timestamp(0).unix(), (-2_082_844_800, 0));
    assert_eq!(Timestamp::from_unix(0, 123_456).unwrap().unix(), (0, 123_456));
    assert_eq!(Timestamp::from_unix(-2_082_844_801, 0), Err(Error::Invalid));
    assert_eq!(Timestamp::from_unix(i64::MAX, 0), Err(Error::Invalid));
    assert_eq!(Timestamp::from_unix(0, 1_000_000), Err(Error::Invalid));
    assert_eq!(Timestamp::from_legacy(u32::MAX).legacy_seconds(), Ok(u32::MAX));
    assert_eq!(Timestamp(u64::MAX).legacy_seconds(), Err(Error::Invalid));
    assert_eq!(Timestamp::from_unix(Timestamp(u64::MAX).unix().0, Timestamp(u64::MAX).unix().1),
               Ok(Timestamp(u64::MAX)));
}

#[test]
fn byte_contract_versions_dirty_state_and_failed_undo()
{
    let d = formatted();
    let header: [u8; 512] = d.bytes[1024..1536].try_into().unwrap();
    assert_eq!(&header[..12], &[b'H', b'B', 0, 1, 0, 0, 16, 0, 0, 17, 0, 0]);
    // Independent Python struct/zlib golden for the fixed formatter inputs.
    assert_eq!(&header[508..], &0x64dd0143u32.to_be_bytes());
    let mut unsupported = header;
    unsupported[3] = 2;
    assert_eq!(layout::Header::decode(&unsupported), Err(Error::Incompatible));
    let mut wrong_unicode = header;
    wrong_unicode[9] = 18;
    assert_eq!(layout::Header::decode(&wrong_unicode), Err(Error::Incompatible));
    let mut dirty = layout::Header::decode(&header).unwrap();
    dirty.flags = layout::DIRTY;
    let mut disk = d.clone();
    let bytes = dirty.encode().unwrap();
    disk.bytes[1024..1536].copy_from_slice(&bytes);
    let end = disk.bytes.len() - 1024;
    disk.bytes[end..end + 512].copy_from_slice(&bytes);
    let p = disk.partition();
    assert!(matches!(Volume::mount(&mut disk, p, Budget::default()), Err(Error::Dirty)));
    let mut disk = d.clone();
    let id = mount(&mut disk).create(2, "stable", false, Timestamp(0)).unwrap().id;
    // Fail after both dirty headers are durable, then keep failing during undo.
    disk.fail_at = Some(disk.writes + 12);
    disk.fail_persistent = true;
    assert_eq!(mount(&mut disk).write_fork(id, ForkKind::Data, 0, b"test", Timestamp(0))
                               .unwrap_err(),
               Error::RollbackFailed);
    disk.fail_at = None;
    disk.fail_persistent = false;
    let p = disk.partition();
    assert!(matches!(Volume::mount(&mut disk, p, Budget::default()), Err(Error::Dirty)));
    for fail in [1, 3, 7, 20, 40]
    {
        let mut disk = Disk::new();
        disk.fail_at = Some(fail);
        let p = disk.partition();
        let result = format(&mut disk,
                            p,
                            FormatOptions { name:    "incomplete",
                                            created: Timestamp(0),
                                            uuid:    [0; 16] },
                            Budget::default());
        if result.is_err()
        {
            assert!(Volume::mount(&mut disk, p, Budget::default()).is_err());
        }
    }
}
