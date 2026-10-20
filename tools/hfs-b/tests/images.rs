use std::{fs, io::Cursor, path::PathBuf};

use hfs_b::*;
use mac_hfs_b::{time::Timestamp, *};
struct Scratch(PathBuf);
impl Drop for Scratch
{
    fn drop(&mut self)
    {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn real_image_streaming_macbinary_import_and_independent_remount()
{
    let root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build/system7/hfs-b-tool-tests");
    fs::create_dir_all(&root).unwrap();
    let scratch = Scratch(root.join(format!("{}", std::process::id())));
    fs::create_dir(&scratch.0).unwrap();
    let path = scratch.0.join("native.hfsb");
    let file = mac_formats::macbinary::MacFile { name:      b"Application".to_vec(),
                                                 file_type: *b"APPL",
                                                 creator:   *b"RUST",
                                                 flags:     0x1234,
                                                 created:   3_875_000_000,
                                                 modified:  3_875_000_001,
                                                 data:      b"data fork".to_vec(),
                                                 resources: vec![0x55; 5 * 1024 * 1024 + 17] };
    let id;
    {
        let mut disk = Image::create(&path, 32 * 1024 * 1024).unwrap();
        let p = disk.partition();
        format(&mut disk,
               p,
               FormatOptions { name:    "System RISC",
                               created: Timestamp(42),
                               uuid:    [1; 16] },
               Budget::default()).unwrap();
        let mut v = Volume::mount(&mut disk, p, Budget::default()).unwrap();
        let system = v.create(2, "System Folder", true, Timestamp(42)).unwrap();
        for name in ["Drivers", "Extensions", "Fonts", "Control Panels"]
        {
            v.create(system.id, name, true, Timestamp(42)).unwrap();
        }
        v.bless(system.id, Timestamp(42)).unwrap();
        let copied_at = Timestamp::from_unix(1_792_528_000, 123_456).unwrap();
        id = import_macbinary_at(&mut v, system.id, &file, "Application", copied_at).unwrap().id;
        assert_eq!(v.entry(system.id).unwrap().modified,
                   copied_at,
                   "copying an old application modifies its destination now");
        assert_eq!(v.header().modified, copied_at.0);
        assert_eq!(v.entry(id).unwrap().created, Timestamp::from_legacy(file.created));
        assert_eq!(v.entry(id).unwrap().modified, Timestamp::from_legacy(file.modified));
        let e = v.entry(id).unwrap();
        assert_eq!(&e.finder_info[..4], b"APPL");
        let mut output = Vec::new();
        read_stream(&mut v, id, ForkKind::Resource, &mut output).unwrap();
        assert_eq!(output, file.resources);
        let target = v.create(2, "脚本 😀", false, Timestamp(123_456)).unwrap();
        write_stream(&mut v,target.id,ForkKind::Data,Cursor::new(b"hello"),Timestamp(123_457)).unwrap();
        v.check().unwrap();
    }
    let mut disk = Image::open(&path, false).unwrap();
    let p = disk.partition();
    let mut v = Volume::mount(&mut disk, p, Budget::default()).unwrap();
    let mut bytes = Vec::new();
    read_stream(&mut v, id, ForkKind::Data, &mut bytes).unwrap();
    assert_eq!(bytes, file.data);
    assert_eq!(v.entry(id).unwrap().resources.length, 5 * 1024 * 1024 + 17);
    v.check().unwrap();
}
