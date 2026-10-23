use std::{fs, path::PathBuf};

use hfs_b::{Image,
            copy::{copy_in, copy_out, resolve},
            read_stream};
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
fn preferences_documents_and_application_forks_roundtrip_into_fresh_image()
{
    let scratch =
        Scratch(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build/system7/\
                                                                hfs-b-tool-tests")
                                                         .join(format!("copy-{}",
                                                                       std::process::id())));
    fs::create_dir_all(&scratch.0).unwrap();
    let host = scratch.0.join("Documents");
    fs::create_dir(&host).unwrap();
    fs::write(host.join("Résumé 😀.txt"), b"my document\n").unwrap();
    fs::create_dir(host.join("Subfolder")).unwrap();
    fs::write(host.join("Subfolder/note.txt"), b"nested").unwrap();
    let time = Timestamp(4_600_000_000_123_456);
    for (index, name) in ["old", "fresh"].into_iter().enumerate()
    {
        let mut disk =
            Image::create(&scratch.0.join(format!("{name}.hfsb")), 16 * 1024 * 1024).unwrap();
        let p = disk.partition();
        format(&mut disk,
               p,
               FormatOptions { name:    "System RISC",
                               created: time,
                               uuid:    [index as u8 + 1; 16] },
               Default::default()).unwrap();
        let mut v = Volume::mount(&mut disk, p, Default::default()).unwrap();
        let system = v.create(2, "System Folder", true, time).unwrap();
        v.create(system.id, "Preferences", true, time).unwrap();
        if index == 0
        {
            copy_in(&mut v, 2, &host, false).unwrap();
            let prefs = resolve(&mut v, "/System Folder/Preferences").unwrap();
            let file = v.create(prefs.id, "Terminal", false, time).unwrap();
            v.write_fork(file.id,
                         ForkKind::Data,
                         0,
                         b"{\"version\":1,\"env\":{\"PATH\":\"/Applications\"}}",
                         time)
             .unwrap();
            v.write_fork(file.id, ForkKind::Resource, 0, b"resource fork", time).unwrap();
            let mut info = [0; 32];
            info[..8].copy_from_slice(b"JSONTEDT");
            info[8..10].copy_from_slice(&0x0804u16.to_be_bytes());
            v.set_comment(file.id, "Keep this 😀", time).unwrap();
            v.set_metadata(file.id, 1, info, time, time, Some(Timestamp(0)), time).unwrap();
            copy_out(&mut v, prefs.id, &scratch.0.join("Preferences")).unwrap();
            let documents = resolve(&mut v, "/Documents").unwrap();
            copy_out(&mut v, documents.id, &scratch.0.join("Saved Documents")).unwrap();
            assert!(copy_out(&mut v, prefs.id, &scratch.0.join("Preferences")).is_err(),
                    "exports never overwrite a prior backup");
        }
        else
        {
            let folder = copy_in(&mut v, system.id, &scratch.0.join("Preferences"), false).unwrap();
            assert_eq!(folder.name, "Preferences");
            let file = resolve(&mut v, "/System Folder/Preferences/Terminal").unwrap();
            assert_eq!(file.created, time);
            assert_eq!(file.modified, time);
            assert_eq!(file.backup, Some(Timestamp(0)));
            assert_eq!(file.flags, 1);
            assert_eq!(&file.finder_info[..8], b"JSONTEDT");
            assert_eq!(v.comment(file.id).unwrap(), "Keep this 😀");
            let mut resources = Vec::new();
            read_stream(&mut v, file.id, ForkKind::Resource, &mut resources).unwrap();
            assert_eq!(resources, b"resource fork");
            copy_in(&mut v, 2, &scratch.0.join("Saved Documents"), false).unwrap();
            let doc = resolve(&mut v, "/Documents/résumé 😀.TXT").unwrap();
            let mut bytes = Vec::new();
            read_stream(&mut v, doc.id, ForkKind::Data, &mut bytes).unwrap();
            assert_eq!(bytes, b"my document\n");
            assert!(resolve(&mut v, "/Documents/Subfolder/note.txt").is_ok());
            assert!(copy_in(&mut v, 2, &host, false).is_err(),
                    "replacing existing files requires opt-in");
            fs::write(host.join("Résumé 😀.txt"), b"replacement").unwrap();
            copy_in(&mut v, 2, &host, true).unwrap();
            let doc = resolve(&mut v, "/Documents/Résumé 😀.txt").unwrap();
            let mut bytes = Vec::new();
            read_stream(&mut v, doc.id, ForkKind::Data, &mut bytes).unwrap();
            assert_eq!(bytes, b"replacement");
        }
        v.check().unwrap();
    }
}
