//! Application-owned icon families through the public SDK and real HFS reads.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs,
          path::PathBuf,
          process::Command,
          sync::atomic::{AtomicUsize, Ordering}};

use mac_formats::{macbinary::{self, MacFile},
                  resource::{self, Resource}};
use mac_system::testing::{DiskFaults, with_file_disk};
use mac_toolbox::{Point, Rect, System,
                  quickdraw::{SurfacePort, icons::IconFlags}};

struct Disk
{
    directory: PathBuf,
    path:      PathBuf,
    tools:     PathBuf
}
static NEXT_DISK: AtomicUsize = AtomicUsize::new(0);
impl Drop for Disk
{
    fn drop(&mut self)
    {
        let _ = fs::remove_dir_all(&self.directory);
    }
}
fn r(kind: &[u8; 4], id: i16, data: Vec<u8>) -> Resource
{
    Resource { kind: *kind,
               id,
               name: None,
               attributes: 0,
               data }
}
impl Disk
{
    fn new() -> Self
    {
        let root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
        let directory =
            root.join("results/system7").join(format!("file-icons-{}-{}",
                                                      std::process::id(),
                                                      NEXT_DISK.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(directory.join("state")).unwrap();
        let disk = Self { path: directory.join("disk.hfs"),
                          directory,
                          tools: root.join(".toolchains/hfsutils/rootfs/usr/bin") };
        fs::File::create(&disk.path).unwrap().set_len(8 * 1024 * 1024).unwrap();
        disk.hfs("hformat", &["-l", "Icons", disk.path.to_str().unwrap()]);
        disk.hfs("hmount", &[disk.path.to_str().unwrap()]);
        // Local icon7 -> actual9433, FREF99. An unmapped ID128
        // family must never win. Both applications deliberately reuse9433.
        let bundle = [b"TEST".as_slice(),
                      &[0, 0, 0, 1],
                      b"ICN#",
                      &[0, 0, 0, 7, 0x24, 0xd9],
                      b"FREF",
                      &[0, 0, 0, 0, 0, 99]].concat();
        for (name, white, malformed) in
            [("Black", false, false), ("White", true, false), ("Broken", false, true)]
        {
            let mut large = vec![0xff; 256];
            large[..128].fill(if white { 255 } else { 0 });
            large[128] = 0; // First eight pixels are transparent.
            let mut small = vec![0xff; 64];
            small[..32].fill(if white { 255 } else { 0 });
            small[32] = 0;
            let resources =
                resource::encode(&[r(b"RV64", 0, vec![0xaa; 1024 * 1024]),
                                   r(b"BNDL",
                                     401,
                                     if malformed { vec![0] } else { bundle.clone() }),
                                   r(b"FREF", 99, b"APPL\0\x07\0".to_vec()),
                                   r(b"ICN#", 9433, large),
                                   r(b"ics#", 9433, small),
                                   // Color data overrides the large monochrome image in White.
                                   r(b"icl8", 9433, vec![if white { 0 } else { 255 }; 1024]),
                                   r(b"ICN#", 128, vec![0xff; 256])]).unwrap();
            let path = disk.directory.join(format!("{name}.bin"));
            fs::write(&path,
                      macbinary::encode(&MacFile { name: name.as_bytes().to_vec(),
                                                   file_type: *b"APPL",
                                                   creator: *b"TEST",
                                                   flags: 0,
                                                   created: 0,
                                                   modified: 0,
                                                   data: vec![],
                                                   resources }).unwrap()).unwrap();
            disk.hfs("hcopy", &["-m", path.to_str().unwrap(), ":"]);
        }
        let large = vec![255; 256];
        let small = vec![255; 64];
        let custom = macbinary::encode(&MacFile {
            name: b"Custom Document".to_vec(), file_type: *b"TEXT", creator: *b"TEST",
            flags: 0x0400, created: 0, modified: 0, data: vec![],
            resources: resource::encode(&[r(b"ICN#", -16455, large), r(b"ics#", -16455, small),
                                          r(b"CODE", 1, vec![0x55; 1024 * 1024])]).unwrap()
        }).unwrap();
        let custom_path = disk.directory.join("custom.bin");
        fs::write(&custom_path, custom).unwrap();
        disk.hfs("hcopy", &["-m", custom_path.to_str().unwrap(), ":"]);
        disk.hfs("humount", &[]);
        disk
    }

    fn hfs(&self, name: &str, args: &[&str])
    {
        let result = Command::new(self.tools.join(name)).args(args)
                                                        .env("HOME", self.directory.join("state"))
                                                        .output()
                                                        .unwrap();
        assert!(result.status.success(), "{name}: {}", String::from_utf8_lossy(&result.stderr));
    }
}
#[test]
fn application_icons_use_their_own_bundle_family_sizes_colors_and_masks()
{
    let disk = Disk::new();
    let before = fs::read(&disk.path).unwrap();
    let (_, activity) =
        with_file_disk(&disk.path, true, DiskFaults::default(), |services| {
            let system = unsafe { System::from_raw(services) }.unwrap();
            let before_count = system.quickdraw().icons().count().unwrap();
            for (name, large, small) in [("Black", 0, 0xffffff), ("White", 0xffffff, 0)]
            {
                let file = system.files()
                                 .lookup(system.files().volume_at(0).unwrap().id, 2, name)
                                 .unwrap();
                for (size, color) in [(32, large), (16, small)]
                {
                    let mut pixels = vec![0x123456; 40 * 40];
                    let mut surface = mac_abi::Surface { pixels: pixels.as_mut_ptr(),
                                                         width:  40,
                                                         height: 40,
                                                         origin: Point::default(),
                                                         clip:   Rect::new(0, 0, 40, 40) };
                    let connection =
                        unsafe { mac_traps::SurfaceConnection::new(&mut surface, services.trap) };
                    SurfacePort::new(connection).plot_file_icon(system.files()
                                                                      .volume_at(0)
                                                                      .unwrap()
                                                                      .id,
                                                                file.id,
                                                                Rect::new(4,
                                                                          4,
                                                                          4 + size,
                                                                          4 + size),
                                                                IconFlags::default())
                                                .unwrap();
                    for y in 0..40
                    {
                        for x in 0..40
                        {
                            let expected = if (4..4 + size as usize).contains(&x)
                                              && (4..4 + size as usize).contains(&y)
                                              && !(y == 4 && x < 12)
                            {
                                color
                            }
                            else
                            {
                                0x123456
                            };
                            assert_eq!(pixels[y * 40 + x], expected, "{name} size{size} at{x},{y}");
                        }
                    }
                }
            }
            let broken = system.files()
                               .lookup(system.files().volume_at(0).unwrap().id, 2, "Broken")
                               .unwrap();
            let mut pixels = vec![0x123456; 32 * 32];
            let mut surface = mac_abi::Surface { pixels: pixels.as_mut_ptr(),
                                                 width:  32,
                                                 height: 32,
                                                 origin: Point::default(),
                                                 clip:   Rect::new(0, 0, 32, 32) };
            let connection =
                unsafe { mac_traps::SurfaceConnection::new(&mut surface, services.trap) };
            let mut port = SurfacePort::new(connection);
            assert_eq!(port.plot_file_icon(system.files().volume_at(0).unwrap().id,
                                           broken.id,
                                           Rect::new(0, 0, 32, 32),
                                           IconFlags::default())
                           .unwrap_err()
                           .0,
                       mac_abi::BAD_ARGUMENT);
            assert_eq!(port.plot_file_icon(system.files().volume_at(0).unwrap().id,
                                           2,
                                           Rect::new(0, 0, 32, 32),
                                           IconFlags::default())
                           .unwrap_err()
                           .0,
                       mac_abi::NOT_FOUND);
            assert_eq!(pixels, vec![0x123456; 32 * 32], "failed lookup must not paint");
            assert_eq!(system.quickdraw().icons().count().unwrap(),
                       before_count,
                       "file lookup must not change resource search chain");
        }).unwrap();
    assert_eq!(activity.writes, 0);
    assert!(activity.reads < 256, "read executable bytes: {activity:?}");
    assert_eq!(fs::read(&disk.path).unwrap(), before);
}

#[test]
fn custom_document_icons_are_read_without_bundle_or_executable_loading()
{
    let disk = Disk::new();
    let (_, activity) =
        with_file_disk(&disk.path, true, DiskFaults::default(), |services| {
            let system = unsafe { System::from_raw(services) }.unwrap();
            let file = system.files()
                             .lookup(system.files().volume_at(0).unwrap().id, 2, "Custom Document")
                             .unwrap();
            assert_eq!(file.file_type, *b"TEXT");
            for size in [16, 32]
            {
                let mut pixels = vec![0x123456; size * size];
                let mut surface = mac_abi::Surface { pixels: pixels.as_mut_ptr(),
                                                     width:  size as u32,
                                                     height: size as u32,
                                                     origin: Point::default(),
                                                     clip:   Rect::new(0, 0, size as i16,
                                                                       size as i16) };
                let connection =
                    unsafe { mac_traps::SurfaceConnection::new(&mut surface, services.trap) };
                SurfacePort::new(connection).plot_file_icon(system.files()
                                                                  .volume_at(0)
                                                                  .unwrap()
                                                                  .id,
                                                            file.id,
                                                            Rect::new(0, 0, size as i16,
                                                                      size as i16),
                                                            IconFlags::default())
                                            .unwrap();
                assert!(pixels.iter().all(|&pixel| pixel == 0),
                        "custom{size}px document icon was not used");
            }
        }).unwrap();
    assert_eq!(activity.writes, 0);
    assert!(activity.reads < 128, "custom lookup read executable payload: {activity:?}");
}
