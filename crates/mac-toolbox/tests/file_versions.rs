//! File-backed metadata through the public Resource Manager and actual HFS I/O.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs,
          path::PathBuf,
          process::Command,
          sync::atomic::{AtomicUsize, Ordering}};

use mac_system::testing::{DiskFaults, with_file_disk};
use mac_toolbox::{ResourceKey, System};
static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Disk
{
    root:      PathBuf,
    directory: PathBuf,
    path:      PathBuf
}
impl Disk
{
    fn new() -> Self
    {
        let root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
        let directory =
            root.join("results/system7").join(format!("file-versions-{}-{}",
                                                      std::process::id(),
                                                      NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(directory.join("hfs-state")).unwrap();
        let path = directory.join("disk.hfs");
        fs::File::create(&path).unwrap().set_len(4 * 1024 * 1024).unwrap();
        let fixture = Self { root,
                             directory,
                             path };
        fixture.hfs("hformat", &["-l", "Versions", fixture.path.to_str().unwrap()]);
        fixture.hfs("hmount", &[fixture.path.to_str().unwrap()]);
        let version =
            [0x12, 0x34, 0x80, 0, 0, 0, 4, b'C', b'a', b'f', 0x8e, 5, b'A', b'p', b'p', b'l', b'e'];
        let resources = mac_formats::resource::encode(&[
            mac_formats::resource::Resource { kind:*b"RV64",id:0,name:None,attributes:0,data:vec![0xaa;1024*1024]},
            mac_formats::resource::Resource { kind:*b"vers",id:1,name:None,attributes:0,data:version.to_vec()},
            mac_formats::resource::Resource { kind:*b"vers",id:2,name:None,attributes:0,data:vec![0;6]},
        ]).unwrap();
        for (name, bytes) in [("Sample", resources), ("Malformed", vec![0; 16])]
        {
            let file = fixture.directory.join(format!("{name}.bin"));
            fs::write(&file,mac_formats::macbinary::encode(&mac_formats::macbinary::MacFile {
                name:name.as_bytes().to_vec(), file_type:*b"APPL",creator:*b"TEST",flags:0,
                created:0,modified:0,data:vec![],resources:bytes
            }).unwrap()).unwrap();
            fixture.hfs("hcopy", &["-m", file.to_str().unwrap(), ":"]);
        }
        fixture.hfs("humount", &[]);
        fixture
    }

    fn hfs(&self, name: &str, args: &[&str])
    {
        let result = Command::new(self.root.join(".toolchains/hfsutils/rootfs/usr/bin").join(name))
            .args(args).env("HOME",self.directory.join("hfs-state")).output().unwrap();
        assert!(result.status.success(), "{name}: {}", String::from_utf8_lossy(&result.stderr));
    }
}

#[test]
fn legacy_info_mutations_convert_strings_only_at_the_trap_boundary()
{
    let fixture = Disk::new();
    with_file_disk(&fixture.path, false, DiskFaults::default(), |services| {
        use mac_abi::files::*;
        let system = unsafe { System::from_raw(services) }.unwrap();
        let original =
            system.files()
                  .lookup(system.files().volume_at(0).unwrap().id, mac_toolbox::Cnid(2), "Sample")
                  .unwrap();
        let name = b"\x04Caf\x8e";
        let mut call = FileCall { operation: RENAME,
                                  volume: (system.files().volume_at(0).unwrap().id).0,
                                  cnid: (original.id).0,
                                  name: name.as_ptr(),
                                  name_length: name.len(),
                                  position: 3_875_209_985,
                                  ..Default::default() };
        let invoke = |call: &mut FileCall| unsafe {
            (services.trap)(mac_abi::traps::PSTRING_FILE,
                            core::ptr::from_mut(call).cast(),
                            size_of::<FileCall>())
        };
        assert_eq!(invoke(&mut call), 0);
        assert_eq!(&call.entry.name[..call.entry.name_length as usize], name);
        assert_eq!(call.name, name.as_ptr());
        assert_eq!(call.name_length, name.len());
        assert_eq!(system.files()
                         .catalog(system.files().volume_at(0).unwrap().id, original.id)
                         .unwrap()
                         .name(),
                   "Café");
        let comment = b"\x08Caf\x8e\rTwo";
        call.operation = SET_COMMENT;
        call.name = comment.as_ptr();
        call.name_length = comment.len();
        assert_eq!(invoke(&mut call), 0);
        let mut native = String::new();
        system.files()
              .comment(system.files().volume_at(0).unwrap().id, original.id, &mut native)
              .unwrap();
        assert_eq!(native, "Café\rTwo");
        let mut output = [0; 256];
        call.operation = COMMENT;
        call.output = output.as_mut_ptr();
        call.capacity = output.len();
        assert_eq!(invoke(&mut call), 0);
        assert_eq!(&output[..call.transferred], comment);
        assert_eq!(call.output, output.as_mut_ptr());
        call.operation = SET_STATIONERY;
        call.index = 1;
        assert_eq!(invoke(&mut call), 0);
        assert_eq!(&call.entry.name[..call.entry.name_length as usize], name);
        call.operation = SET_LOCKED;
        assert_eq!(invoke(&mut call), 0);
        assert_eq!(&call.entry.name[..call.entry.name_length as usize], name);
        assert!(system.files()
                      .catalog(system.files().volume_at(0).unwrap().id, original.id)
                      .unwrap()
                      .is_locked());
    }).unwrap();
    fs::remove_file(fixture.path).unwrap();
}

#[test]
fn arbitrary_file_versions_are_utf8_scoped_and_read_only()
{
    let fixture = Disk::new();
    let before = fs::read(&fixture.path).unwrap();
    let (_, activity) =
        with_file_disk(&fixture.path, true, DiskFaults::default(), |services| {
            let system = unsafe { System::from_raw(services) }.unwrap();
            let file =
                system.files()
                      .lookup(system.files().volume_at(0).unwrap().id,
                              mac_toolbox::Cnid(2),
                              "Sample")
                      .unwrap();
            let count =
                system.resources().system().count(mac_toolbox::TypeCode::new(*b"vers")).unwrap();
            let version = system.resources()
                                .file_version(system.files().volume_at(0).unwrap().id, file.id, 1)
                                .unwrap();
            assert_eq!(version.short(), "Café");
            assert_eq!(version.long(), "Apple");
            assert_eq!(version.number, [0x12, 0x34, 0x80, 0]);
            assert_eq!(system.resources()
                             .system()
                             .count(mac_toolbox::TypeCode::new(*b"vers"))
                             .unwrap(),
                       count);
            assert_eq!(system.resources()
                             .file_version(system.files().volume_at(0).unwrap().id, file.id, 3)
                             .unwrap_err()
                             .status(),
                       mac_abi::NOT_FOUND);
            assert_eq!(system.resources()
                             .file_version(system.files().volume_at(0).unwrap().id, file.id, 2)
                             .unwrap_err()
                             .status(),
                       mac_abi::BAD_ARGUMENT);
            let bad = system.files()
                            .lookup(system.files().volume_at(0).unwrap().id,
                                    mac_toolbox::Cnid(2),
                                    "Malformed")
                            .unwrap();
            assert_eq!(system.resources()
                             .file_version(system.files().volume_at(0).unwrap().id, bad.id, 1)
                             .unwrap_err()
                             .status(),
                       mac_abi::BAD_ARGUMENT);
            assert_eq!(system.resources()
                             .file_version(system.files().volume_at(0).unwrap().id,
                                           mac_toolbox::Cnid(2),
                                           1)
                             .unwrap_err()
                             .status(),
                       mac_abi::NOT_FOUND);
            let mut call =
                mac_abi::resources::FileVersionCall { operation:
                                                          mac_abi::resources::FILE_VERSION,
                                                      volume: (system.files()
                                                                     .volume_at(0)
                                                                     .unwrap()
                                                                     .id)
                                                                         .0,
                                                      cnid: (file.id).0,
                                                      id: 1,
                                                      ..Default::default() };
            let status = unsafe {
                (services.trap)(mac_abi::traps::PSTRING_RESOURCE,
                                core::ptr::from_mut(&mut call).cast(),
                                size_of_val(&call))
            };
            assert_eq!(status, 0);
            assert_eq!(&call.info.short[..call.info.short_length as usize], b"\x04Caf\x8e");
            assert_eq!(&call.info.long[..call.info.long_length as usize], b"\x05Apple");
            assert_eq!(system.resources()
                             .get(ResourceKey { kind: mac_toolbox::TypeCode::new(*b"RV64"),
                                                id:   0 })
                             .err()
                             .map(|e| e.status()),
                       Some(mac_abi::NOT_FOUND));
        }).unwrap();
    assert_eq!(activity.writes, 0);
    assert!(activity.reads < 256,
            "metadata requests read executable payload: {} sectors",
            activity.reads);
    assert_eq!(fs::read(&fixture.path).unwrap(), before);
    fs::remove_file(fixture.path).unwrap();
}
