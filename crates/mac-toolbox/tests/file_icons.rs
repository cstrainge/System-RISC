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
                let file =
                    system.files()
                          .lookup(system.files().volume_at(0).unwrap().id,
                                  mac_toolbox::Cnid(2),
                                  name)
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
            let broken =
                system.files()
                      .lookup(system.files().volume_at(0).unwrap().id,
                              mac_toolbox::Cnid(2),
                              "Broken")
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
                           .status(),
                       mac_abi::BAD_ARGUMENT);
            assert_eq!(port.plot_file_icon(system.files().volume_at(0).unwrap().id,
                                           mac_toolbox::Cnid(2),
                                           Rect::new(0, 0, 32, 32),
                                           IconFlags::default())
                           .unwrap_err()
                           .status(),
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
                             .lookup(system.files().volume_at(0).unwrap().id,
                                     mac_toolbox::Cnid(2),
                                     "Custom Document")
                             .unwrap();
            assert_eq!(file.file_type, mac_toolbox::TypeCode::new(*b"TEXT"));
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

fn driver_icon_resources(color: bool) -> Vec<Resource>
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest = mac_build::Manifest::read(&root.join("system/resources.json")).unwrap();
    manifest.resources
            .into_iter()
            .filter_map(|r| {
                let metadata = matches!(r.kind.as_str(), "BNDL" | "FREF");
                let icon = r.id == mac_toolbox::quickdraw::icons::standard::DRIVER
                           && (color || !matches!(r.kind.as_str(), "icl8" | "ics8"));
                let palette = r.kind == "clut" && r.id == 8;
                if !(metadata || icon || palette)
                {
                    return None;
                }
                let mac_build::manifest::Payload::File { path } = r.payload
                else
                {
                    panic!("icon source must be a packaged file")
                };
                Some(Resource { kind:       r.kind.as_bytes().try_into().unwrap(),
                                id:         r.id,
                                name:       None,
                                attributes: r.attributes,
                                data:       fs::read(root.join("system").join(path)).unwrap() })
            })
            .collect()
}
#[test]
fn generic_driver_icon_is_resource_based_and_clear_at_each_native_size_and_depth()
{
    use mac_toolbox::{ResourceScope, TypeCode, quickdraw::icons::standard};
    for color in [false, true]
    {
        let resources = driver_icon_resources(color);
        let encoded = resource::encode(&resources).unwrap();
        let application = resource::encode(&[]).unwrap();
        mac_system::testing::with_resource_forks(&encoded, &application, |services| {
            let system = unsafe { System::from_raw(services) }.unwrap();
            let icons = system.quickdraw().icons();
            assert_eq!(icons.for_file(TypeCode::new(*b"TEST"), TypeCode::new(*b"DRVR")).unwrap(),
                       standard::DRIVER);
            assert_eq!(icons.for_file(TypeCode::new(*b"TEST"), TypeCode::new(*b"INIT")).unwrap(),
                       standard::EXTENSION);
            assert_eq!(icons.application()
                            .for_file(TypeCode::new(*b"TEST"), TypeCode::new(*b"DRVR"))
                            .unwrap_err(),
                       mac_toolbox::Error::NotFound);
            for size in [16usize, 32]
            {
                let mono = resources.iter()
                                    .find(|r| {
                                        r.id == standard::DRIVER
                                        && r.kind == if size == 16 { *b"ics#" } else { *b"ICN#" }
                                    })
                                    .unwrap();
                let pixels_source =
                    resources.iter().find(|r| {
                                        r.id == standard::DRIVER
                                        && r.kind == if size == 16 { *b"ics8" } else { *b"icl8" }
                                    });
                let palette = resources.iter().find(|r| r.kind == *b"clut" && r.id == 8).unwrap();
                let mut pixels = vec![0x123456; 40 * 40];
                let mut surface = mac_abi::Surface { pixels: pixels.as_mut_ptr(),
                                                     width:  40,
                                                     height: 40,
                                                     origin: Point::default(),
                                                     clip:   Rect::new(0, 0, 40, 40) };
                let connection =
                    unsafe { mac_traps::SurfaceConnection::new(&mut surface, services.trap) };
                SurfacePort::new(connection).plot_icon(ResourceScope::System,
                                                       standard::DRIVER,
                                                       Rect::new(4,
                                                                 3,
                                                                 4 + size as i16,
                                                                 3 + size as i16),
                                                       IconFlags::default())
                                            .unwrap();
                for y in 0..40
                {
                    for x in 0..40
                    {
                        let mut expected = 0x123456;
                        if (4..4 + size).contains(&x) && (3..3 + size).contains(&y)
                        {
                            let (ix, iy) = (x - 4, y - 3);
                            let byte = iy * (size / 8) + ix / 8;
                            let mask = 1 << (7 - ix % 8);
                            if mono.data[size * size / 8 + byte] & mask != 0
                            {
                                expected = if let Some(source) = pixels_source
                                {
                                    let index = source.data[iy * size + ix] as usize;
                                    let at = 8 + index * 8;
                                    u32::from(palette.data[at + 2]) << 16
                                    | u32::from(palette.data[at + 4]) << 8
                                    | u32::from(palette.data[at + 6])
                                }
                                else if mono.data[byte] & mask != 0
                                {
                                    0
                                }
                                else
                                {
                                    0xffffff
                                };
                            }
                        }
                        if expected != 0x123456
                        {
                            let display = include_bytes!("../../../system/assets/icons/\
                                                          system7-indexed-display.bin");
                            let record =
                                display.as_chunks::<32>()
                                       .0
                                       .iter()
                                       .find(|row| {
                                           u32::from_be_bytes(row[..4].try_into().unwrap())
                                           == expected
                                       })
                                       .expect("source color absent from original GDevice \
                                                reference");
                            expected = u32::from_be_bytes(record[4..8].try_into().unwrap());
                        }
                        assert_eq!(pixels[y * 40 + x],
                                   expected,
                                   "{size}px color={color} at {x},{y}");
                    }
                }
            }
        }).unwrap();
    }
}
#[test]
fn creator_specific_driver_bundle_wins_without_crossing_resource_fork_mappings()
{
    let resources = driver_icon_resources(true);
    let system = resource::encode(&resources).unwrap();
    let app = resource::encode(&[r(b"BNDL",
                                   0,
                                   [b"TEST".as_slice(),
                                    &[0, 0, 0, 1],
                                    b"ICN#",
                                    &[0, 0, 0, 7, 0x24, 0xd9],
                                    b"FREF",
                                    &[0, 0, 0, 0, 0, 0]].concat()),
                                 r(b"FREF", 0, b"DRVR\0\x07\0".to_vec())]).unwrap();
    mac_system::testing::with_resource_forks(&system, &app, |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let icons = system.quickdraw().icons();
        assert_eq!(icons.for_file(mac_toolbox::TypeCode::new(*b"TEST"),
                                  mac_toolbox::TypeCode::new(*b"DRVR"))
                        .unwrap(),
                   9433);
        assert_eq!(icons.system()
                        .for_file(mac_toolbox::TypeCode::new(*b"TEST"),
                                  mac_toolbox::TypeCode::new(*b"DRVR"))
                        .unwrap(),
                   mac_toolbox::quickdraw::icons::standard::DRIVER);
        assert_eq!(icons.for_file(mac_toolbox::TypeCode::new(*b"MACS"),
                                  mac_toolbox::TypeCode::new(*b"INIT"))
                        .unwrap(),
                   mac_toolbox::quickdraw::icons::standard::EXTENSION);
    }).unwrap();
}

#[test]
fn native_system_icon_reservation_preserves_classic_ids_and_rejects_import_collisions()
{
    use mac_toolbox::quickdraw::icons::standard;
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("data/classic-system-icon-ids.json")).unwrap();
    assert_eq!(standard::DRIVERS_FOLDER, -30000);
    assert_eq!(standard::DRIVER, -30001);
    let native = -30031..=-30000;
    for source in reference["sources"].as_array().unwrap()
    {
        for id in source["icon_resource_ids"].as_array().unwrap()
        {
            let id = id.as_i64().unwrap() as i16;
            assert!(!native.contains(&id),
                    "reserved System RISC block collides with {}: {id}",
                    source["name"]);
        }
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut manifest = mac_build::Manifest::read(&root.join("system/resources.json")).unwrap();
    for r in &manifest.resources
    {
        if native.contains(&r.id)
        {
            assert!((r.id == standard::DRIVERS_FOLDER && r.symbol.starts_with("DRIVERS_FOLDER_"))
                    || (r.id == standard::DRIVER
                        && (r.symbol.starts_with("DRIVER_FILE_")
                            || r.symbol.starts_with("NATIVE_DRIVER_ICON_"))),
                    "unexpected resource in native System icon reservation: {} {}",
                    r.kind,
                    r.id);
        }
    }
    // The build compiler refuses a later classic import that uses an occupied
    // family key, instead of silently replacing the native or original payload.
    let mut collision = mac_build::Manifest::read(&root.join("system/resources.json")).unwrap();
    let at = collision.resources
                      .iter()
                      .position(|r| r.kind == "ICN#" && r.id == standard::DRIVER)
                      .unwrap();
    let mut imported = collision.resources.remove(at);
    imported.symbol = "ORIGINAL_VISUAL_IMPORTED".into();
    manifest.resources.push(imported);
    assert!(manifest.validate().is_err());
}
