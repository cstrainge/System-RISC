//! Native application ownership through the public SDK and real disk service.
//! Context selection/exit are trusted host scheduler fixtures, not ABI fields.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs,
          path::PathBuf,
          process::Command,
          sync::atomic::{AtomicUsize, Ordering}};

use mac_abi::files::*;
use mac_system::testing::{files::{close_owner, with_owner},
                          with_disk};
use mac_toolbox::{Error, System, abi::ACCESS_DENIED};
static NEXT: AtomicUsize = AtomicUsize::new(0);

fn disk() -> Vec<u8>
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let dir = root.join("results/system7").join(format!("file-ownership-{}-{}",
                                                        std::process::id(),
                                                        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir_all(dir.join("hfs-state")).unwrap();
    let raw = dir.join("ownership.hfs");
    fs::File::create(&raw).unwrap().set_len(1024 * 1024).unwrap();
    let sample = dir.join("sample.bin");
    fs::write(&sample, mac_formats::macbinary::encode(&mac_formats::macbinary::MacFile {
        name: b"Caf\x8e".to_vec(), file_type: *b"TEXT", creator: *b"TEST", flags: 0,
        created: 0, modified: 0, data: (0..64).collect(), resources: b"RESOURCE".to_vec()
    }).unwrap()).unwrap();
    let hfs = |tool: &str, args: &[&str]| {
        let output = Command::new(root.join(".toolchains/hfsutils/rootfs/usr/bin").join(tool))
            .args(args).env("HOME", dir.join("hfs-state")).env("TZ", "UTC").output().unwrap();
        assert!(output.status.success(), "{tool}: {}", String::from_utf8_lossy(&output.stderr));
    };
    hfs("hformat", &["-l", "Ownership", raw.to_str().unwrap()]);
    hfs("hmount", &[raw.to_str().unwrap()]);
    hfs("hmkdir", &[":Folder"]);
    hfs("hcopy", &["-m", sample.to_str().unwrap(), ":Folder:"]);
    hfs("humount", &[]);
    fs::read(raw).unwrap()
}

fn with_system(run: impl FnOnce(&System<'_>))
{
    with_disk(&disk(), |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        run(&system);
    });
}

fn open(system: &System<'_>, owner: u64) -> u32
{
    with_owner(owner, || {
        let entry = system.files()
                          .lookup(system.files().volume_at(0).unwrap().id,
                                  mac_toolbox::Cnid(ROOT_ID),
                                  "Folder/Café")
                          .unwrap();
        let mut call = FileCall { operation: OPEN,
                                  volume: (system.files().volume_at(0).unwrap().id).0,
                                  cnid: (entry.id).0,
                                  fork: DATA_FORK,
                                  ..Default::default() };
        system.client().file(&mut call, "", &mut []).unwrap();
        call.handle
    })
}

#[test]
fn foreign_fork_handles_cannot_read_seek_tell_or_close()
{
    with_system(|system| {
        let handle = open(system, 11);
        let client = system.client();
        let mut seek = FileCall { operation: SEEK,
                                  handle,
                                  offset: 7,
                                  whence: SEEK_START,
                                  ..Default::default() };
        with_owner(11, || client.file(&mut seek, "", &mut [])).unwrap();
        for operation in [READ, SEEK, TELL, CLOSE]
        {
            let mut output = [0xa5; 16];
            let mut call = FileCall { operation,
                                      handle,
                                      offset: 2,
                                      position: 99,
                                      transferred: 99,
                                      ..Default::default() };
            assert_eq!(with_owner(22, || client.file(&mut call, "", &mut output)),
                       Err(Error::AccessDenied),
                       "operation {operation}");
            assert_eq!(output, [0xa5; 16], "foreign read must preserve the destination");
            assert_eq!(call.transferred, 0);
            assert_eq!(call.position, 99);
        }
        let mut tell = FileCall { operation: TELL,
                                  handle,
                                  ..Default::default() };
        with_owner(11, || client.file(&mut tell, "", &mut [])).unwrap();
        assert_eq!(tell.position, 7, "rejected foreign operations preserve the owner's mark");
        assert_eq!(close_owner(11), 1);
    });
}

#[test]
fn legacy_conversion_gate_enforces_the_same_trusted_owner()
{
    with_disk(&disk(), |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let handle = open(&system, 11);
        let mut call = FileCall { operation: TELL,
                                  handle,
                                  ..Default::default() };
        let invoke = |call: &mut FileCall| unsafe {
            (services.trap)(mac_abi::traps::PSTRING_FILE,
                            core::ptr::from_mut(call).cast(),
                            size_of::<FileCall>())
        };
        assert_eq!(with_owner(22, || invoke(&mut call)), i64::from(ACCESS_DENIED));
        assert_eq!(with_owner(11, || invoke(&mut call)), 0);
        assert_eq!(call.position, 0);
        assert_eq!(close_owner(11), 1);
    });
}

#[test]
fn exit_releases_only_its_handles_and_old_ids_do_not_alias_relaunch()
{
    with_system(|system| {
        let surviving = open(system, 22);
        let mut exited = Vec::new();
        for _ in 0..63
        {
            exited.push(open(system, 11));
        }
        let entry = system.files()
                          .lookup(system.files().volume_at(0).unwrap().id,
                                  mac_toolbox::Cnid(ROOT_ID),
                                  "Folder/Café")
                          .unwrap();
        let mut call = FileCall { operation: OPEN,
                                  volume: (system.files().volume_at(0).unwrap().id).0,
                                  cnid: (entry.id).0,
                                  fork: DATA_FORK,
                                  ..Default::default() };
        assert_eq!(with_owner(33, || system.client().file(&mut call, "", &mut [])),
                   Err(Error::NoMemory));
        assert_eq!(close_owner(11), 63);
        assert_eq!(close_owner(11), 0, "exit cleanup is idempotent");
        let replacement = open(system, 33);
        assert!(!exited.contains(&replacement));
        for handle in exited
        {
            let mut stale = FileCall { operation: TELL,
                                       handle,
                                       ..Default::default() };
            assert_eq!(with_owner(11, || system.client().file(&mut stale, "", &mut [])),
                       Err(Error::NotFound));
            assert_eq!(with_owner(33, || system.client().file(&mut stale, "", &mut [])),
                       Err(Error::NotFound));
        }
        let mut own = FileCall { operation: READ,
                                 handle: surviving,
                                 ..Default::default() };
        let mut output = [0; 8];
        with_owner(22, || system.client().file(&mut own, "", &mut output)).unwrap();
        assert_eq!(own.transferred, 8, "another app's fork remains open across exit");
        assert_eq!(own.position, 8);
        assert_eq!(close_owner(33), 1);
        assert_eq!(close_owner(22), 1);
    });
}

#[test]
fn separate_apps_have_independent_marks_and_foreign_sdk_values_are_rejected()
{
    with_system(|system| {
        let entry = system.files()
                          .lookup(system.files().volume_at(0).unwrap().id,
                                  mac_toolbox::Cnid(ROOT_ID),
                                  "Folder/Café")
                          .unwrap();
        let mut first = with_owner(11, || {
                            system.files().open(system.files().volume_at(0).unwrap().id,
                                                entry.id,
                                                mac_toolbox::files::Fork::Data)
                        }).unwrap();
        let mut second = with_owner(22, || {
                             system.files().open(system.files().volume_at(0).unwrap().id,
                                                 entry.id,
                                                 mac_toolbox::files::Fork::Data)
                         }).unwrap();
        let mut bytes = [0; 8];
        assert_eq!(with_owner(11, || first.read(&mut bytes)), Ok(8));
        assert_eq!(with_owner(22, || second.position()), Ok(0));
        assert_eq!(with_owner(22, || second.read(&mut bytes)), Ok(8));
        assert_eq!(with_owner(22, || first.position()), Err(Error::AccessDenied));
        assert_eq!(with_owner(11, || first.position()), Ok(8));
        assert_eq!(with_owner(22, || first.close()), Err(Error::AccessDenied));
        assert_eq!(with_owner(11, || first.position()), Ok(8), "failed close retains the owner");
        with_owner(11, || first.close()).unwrap();
        with_owner(11, || first.close()).unwrap();
        with_owner(22, || second.close()).unwrap();
        assert_eq!(close_owner(11), 0);
        assert_eq!(close_owner(22), 0);
    });
}
