//! Real boot rasters and volume Info compared with public SDK rendering.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs,
          path::{Path, PathBuf},
          process::Command};

use mac_toolbox::{Point, Rect,
                  quickdraw::{SurfacePort, font}};
fn frame(path: &Path) -> Vec<u8>
{
    let bytes = fs::read(path).unwrap();
    let header = b"P6\n640 480\n255\n";
    assert!(bytes.starts_with(header));
    assert_eq!(bytes.len(), header.len() + 640 * 480 * 3);
    bytes[header.len()..].to_vec()
}
fn region(bytes: &[u8], left: usize, top: usize, width: usize, height: usize) -> Vec<u8>
{
    (top..top + height).flat_map(|y| {
                           bytes[(y * 640 + left) * 3..(y * 640 + left + width) * 3].iter().copied()
                       })
                       .collect()
}
fn same(actual: Vec<u8>, expected: Vec<u8>, label: &str)
{
    assert_eq!(actual.len(), expected.len(), "{label}: dimensions");
    if let Some(i) = actual.iter().zip(&expected).position(|(a, b)| a != b)
    {
        panic!("{label}: pixel {} differs: actual {:?}, expected {:?}",
               i / 3,
               &actual[i / 3 * 3..i / 3 * 3 + 3],
               &expected[i / 3 * 3..i / 3 * 3 + 3]);
    }
}
fn render(root: &Path, width: usize, height: usize, mut pixels: Vec<u32>,
          draw: impl FnOnce(&mut SurfacePort<'_>))
          -> Vec<u8>
{
    let system = fs::read(root.join("build/system7/dist/System.rsrc")).unwrap();
    let application = mac_formats::resource::encode(&[]).unwrap();
    mac_system::testing::with_resource_forks(&system, &application, |services| {
        let mut surface = mac_abi::Surface { pixels: pixels.as_mut_ptr(),
                                             width:  width as u32,
                                             height: height as u32,
                                             origin: Point::default(),
                                             clip:   Rect::new(0, 0, width as i16, height as i16) };
        let connection = unsafe { mac_traps::SurfaceConnection::new(&mut surface, services.trap) };
        draw(&mut SurfacePort::new(connection));
    }).unwrap();
    pixels.into_iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, p as u8]).collect()
}
#[test]
#[ignore = "requires current HFS-B image and private graphical QEMU"]
fn boot_art_panel_and_mounted_volume_info_match_public_toolbox_services()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output = root.join("results/system7").join(format!("boot-ui-{}", std::process::id()));
    let run = Command::new("python3").arg(root.join("System7/scripts/capture_system7_boot.py"))
                                     .arg("--output")
                                     .arg(&output)
                                     .current_dir(&root)
                                     .output()
                                     .unwrap();
    assert!(run.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr));
    for (name, id) in [("happy", 32740), ("missing", 32741)]
    {
        let actual = frame(&output.join(format!("{name}.ppm")));
        let checker =
            (0..32 * 32).map(|n| if (n % 32 + n / 32) & 1 == 0 { 0x878787 } else { 0xc0c0c0 })
                        .collect();
        let expected = render(&root, 32, 32, checker, |p| {
            p.plot_icon(mac_abi::resources::SYSTEM, id, Rect::new(0, 0, 32, 32), Default::default())
             .unwrap()
        });
        assert_eq!(region(&actual, 304, 224, 32, 32),
                   expected,
                   "{name} ROM art differs from System's public icon resource");
        for y in 0..64
        {
            for x in 0..64
            {
                let value = if (x + y) & 1 == 0 { 0x87 } else { 0xc0 };
                assert_eq!(&actual[(y * 640 + x) * 3..(y * 640 + x) * 3 + 3],
                           &[value; 3],
                           "boot background differs from desktop pattern");
            }
        }
    }
    let panel = frame(&output.join("panel.ppm"));
    assert_eq!(region(&panel, 170, 150, 300, 1), vec![0; 300 * 3]);
    assert_eq!(region(&panel, 174, 326, 300, 4), vec![0; 300 * 4 * 3], "panel shadow");
    let happy = render(&root, 64, 64, vec![0xffffff; 64 * 64], |p| {
        p.plot_icon(mac_abi::resources::SYSTEM, 32740, Rect::new(0, 0, 64, 64), Default::default())
         .unwrap()
    });
    assert_eq!(region(&panel, 288, 170, 64, 64), happy);
    let title = render(&root, 296, 20, vec![0xffffff; 296 * 20], |p| {
        let width = p.styled_width(0, "Welcome to System RISC").unwrap();
        p.styled_text(0, 148 - width / 2, 19, "Welcome to System RISC", 0).unwrap();
    });
    same(region(&panel, 172, 252, 296, 20), title, "welcome text must use System's font");
    let log = fs::read_to_string(output.join("boot/serial.log")).unwrap();
    for stage in 1..=7
    {
        assert_eq!(log.matches(&format!("BOOT_PROGRESS completed={stage} total=7")).count(), 1);
    }
    assert!(log.contains("FINDER_INFO_OPEN"));
    let info = frame(&output.join("volume-info.ppm"));
    let kind = render(&root, 163, 12, vec![0xffffff; 163 * 12], |p| {
        p.draw_text(font::GENEVA_9, 0, 10, "HFS-B, version 1", 163, 0).unwrap()
    });
    same(region(&info, 79, 91, 163, 12), kind, "volume Info filesystem/version text");
    let icon = render(&root, 32, 32, vec![0xffffff; 32 * 32], |p| {
        p.plot_icon(mac_abi::resources::SYSTEM, 24000, Rect::new(0, 0, 32, 32), Default::default())
         .unwrap()
    });
    same(region(&info, 38, 53, 32, 32), icon, "volume Info must show its normal driver icon");
    let (v, _) = mac_system::testing::with_file_disk(&root.join("build/system7/dist/system7.hfsb"),
                                                     true,
                                                     Default::default(),
                                                     |services| {
                                                         let system =
                                                    unsafe {
                                                        mac_toolbox::System::from_raw(services)
                                                    }.unwrap();
                                                         system.files().volume_at(0).unwrap()
                                                     }).unwrap();
    assert_eq!(mac_toolbox::files::Filesystem::of(&v).unwrap(),
               mac_toolbox::files::Filesystem::HfsB { version: 1 });
    assert_eq!(v.total_bytes, 500 * 1024 * 1024);
    assert!(v.blessed_folder >= 16);
    let total = render(&root, 163, 12, vec![0xffffff; 163 * 12], |p| {
        p.draw_text(font::GENEVA_9, 0, 10, "500 MB total", 163, 0).unwrap()
    });
    assert_eq!(region(&info, 79, 103, 163, 12), total);
    let tenths = v.free_bytes * 10 / (1024 * 1024);
    let free = if tenths.is_multiple_of(10)
    {
        format!("{} MB free", tenths / 10)
    }
    else
    {
        format!("{}.{} MB free", tenths / 10, tenths % 10)
    };
    let free = render(&root, 163, 12, vec![0xffffff; 163 * 12], |p| {
        p.draw_text(font::GENEVA_9, 0, 10, &free, 163, 0).unwrap()
    });
    same(region(&info, 79, 115, 163, 12),
         free,
         "volume Info free space must match its mounted backend");
}
