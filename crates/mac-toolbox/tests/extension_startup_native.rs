//! Actual optimized System discovery/INIT activation, paused externally through
//! GDB. Public Icon Utilities supply expected pixels; no production worker or
//! host clipboard behavior is claimed by these private startup fixtures.
#![cfg(not(target_arch = "riscv64"))]
#[path = "support/native_capture.rs"]
mod native_capture;

use std::{fs,
          path::{Path, PathBuf}};

use mac_formats::{macbinary, resource};
use mac_toolbox::{Point, Rect, ResourceScope, System, TypeCode,
                  quickdraw::{SurfacePort, icons::IconFlags}};

const CASES: [(&str, bool, bool); 3] = [("Startup Artwork Probe", true, false),
                                        ("Startup Fallback Probe", false, false),
                                        ("Startup Refusal Probe", true, true)];

#[test]
#[ignore = "requires explicitly selected private optimized installation, QEMU and gdb-multiarch"]
fn native_init_outcomes_match_public_icon_services_with_exact_cell_damage()
{
    let build = native_capture::build();
    let output = build.join("checks").join(format!("extension-startup-{}", std::process::id()));
    let result = native_capture::command("capture_extension_startup.py").arg("--output")
                                                                        .arg(&output)
                                                                        .output()
                                                                        .unwrap();
    assert!(result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    assert_capture(&output);
}

#[test]
#[ignore = "validates a completed private GDB/QMP startup-strip capture"]
fn prepared_native_init_outcomes_pass_sdk_and_whole_frame_acceptance()
{
    let output = PathBuf::from(std::env::var_os("SYSTEM7_EXTENSION_STARTUP_CAPTURE")
        .expect("SYSTEM7_EXTENSION_STARTUP_CAPTURE must select the private capture"));
    assert_capture(&output);
}

fn ppm(path: &Path) -> (usize, usize, Vec<u32>)
{
    let bytes = fs::read(path).unwrap();
    let mut at = 0;
    let mut token = || {
        while bytes[at].is_ascii_whitespace()
        {
            at += 1;
        }
        let start = at;
        while !bytes[at].is_ascii_whitespace()
        {
            at += 1;
        }
        std::str::from_utf8(&bytes[start..at]).unwrap()
    };
    assert_eq!(token(), "P6");
    let width: usize = token().parse().unwrap();
    let height: usize = token().parse().unwrap();
    assert_eq!(token(), "255");
    let pixels = &bytes[at + 1..];
    assert_eq!(pixels.len(), width * height * 3, "invalid QMP screenshot extent");
    (width,
     height,
     pixels.chunks_exact(3)
           .map(|p| u32::from(p[0]) << 16 | u32::from(p[1]) << 8 | u32::from(p[2]))
           .collect())
}

fn assert_capture(output: &Path)
{
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["scope"], "native-startup-strip-only");
    assert_eq!(report["original_before"], report["original_after"]);
    assert_eq!(report["termination"], "desktop-ready");
    assert_eq!(report["mouse"], "115,96", "physical input after genuine INIT refusal failed");
    assert_eq!(report["cleanup_errors"], serde_json::json!([]));
    assert!(report.get("capture_error").is_none(), "{report}");
    assert!(report.get("media_quarantine").is_none(), "{report}");
    let owners = report["owners"].as_array().unwrap();
    assert_eq!(owners.len(), 2);
    for owner in owners
    {
        assert_eq!(owner["reaped"], true, "{owner}");
        assert_eq!(owner["exit_code"], 0, "{owner}");
    }
    assert_eq!(report["discovery_symbol"], "mac_system::drivers::runtime::discovery::discover");
    assert_eq!(report["before_pc"], report["discovery_entry"]);
    assert_ne!(report["after_pc"], report["before_pc"]);
    let command = report["command"].as_array().unwrap();
    let display = command.iter().position(|argument| argument == "-display").unwrap();
    assert_eq!(command[display + 1], "none");
    assert!(!command.iter().any(|argument| argument.as_str().unwrap().contains("qemu-vdagent")));

    let before_log = fs::read_to_string(output.join("before.serial.log")).unwrap();
    let after_log = fs::read_to_string(output.join("after.serial.log")).unwrap();
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert!(before_log.contains("BOOT_PROGRESS completed=2 total=7"), "{before_log}");
    for log in [&before_log, &after_log]
    {
        assert!(!log.contains("BOOT_PROGRESS completed=3"), "desktop work preceded capture: {log}");
        assert!(!log.contains("DESKTOP_READY"), "capture was taken after startup: {log}");
    }
    assert!(serial.contains("DESKTOP_READY"), "local desktop did not recover: {serial}");
    for failure in ["SYSTEM_PANIC:", "SYSTEM_TRAP:", "DRIVER_PANIC_FATAL:",
                    "SYSTEM_ALLOCATION_FAILED:", "MODULE_INIT_CLEANUP_RETAINED",
                    "EXTENSION_STARTUP_PRESENTATION_REFUSED"]
    {
        assert!(!serial.contains(failure), "{serial}");
    }
    assert_eq!(after_log.lines().filter(|line| line.starts_with("EXTENSION_ICON ")).count(), 3);

    let (width, height, before) = ppm(&output.join("before.ppm"));
    let (after_width, after_height, actual) = ppm(&output.join("after.ppm"));
    assert_eq!((width, height), (after_width, after_height));
    let mut expected = before.clone();
    let system_resources = fs::read(output.join("System.rsrc")).unwrap();
    let mut cells = Vec::new();
    for (ordinal, (name, artwork, refused)) in CASES.into_iter().enumerate()
    {
        assert!(!before_log.contains(&format!("MODULE_ENTER name={name} ")));
        assert!(after_log.contains(&format!("MODULE_ENTER name={name} ")), "{after_log}");
        let ready = format!("MODULE_PUBLISHED name={name} ");
        assert_eq!(after_log.contains(&ready), !refused, "{after_log}");
        let bounds = Rect::new(8 + ordinal as i16 * 40,
                               height as i16 - 40,
                               40 + ordinal as i16 * 40,
                               height as i16 - 8);
        assert!(after_log.contains(&format!("EXTENSION_ICON name={name:?} ordinal={ordinal} \
                                             outcome={} artwork={} bounds={},{},{},{}",
                                            if refused { "refused" } else { "ready" },
                                            if artwork { "package" } else { "theme" },
                                            bounds.left,
                                            bounds.top,
                                            bounds.right,
                                            bounds.bottom)),
                "{after_log}");
        if refused
        {
            assert!(after_log.contains(&format!("MODULE_REJECTED name={name} status=-4")),
                    "a real initializer refusal was not retained: {after_log}");
            let refusal = after_log.lines()
                                   .find(|line| {
                                       line.starts_with(&format!("EXTENSION_STARTUP_REFUSED \
                                                                  name={name:?} "))
                                   })
                                   .unwrap();
            assert!(refusal.contains("cause=") && refusal.contains("next="), "{refusal}");
            assert!(!refusal.ends_with("next="), "{refusal}");
        }
        let file = macbinary::decode(&fs::read(output.join("packages").join(format!("{name}.bin"))).unwrap()).unwrap();
        assert_eq!(file.file_type, *b"INIT");
        let resources = resource::decode(&file.resources).unwrap();
        let package = mac_formats::module::Package::inspect_extension(
            &resources, mac_module::ExtensionTarget::System).unwrap();
        assert_eq!(package.code.descriptor.abi_version, 1);
        assert_eq!(package.contract.code_id, 0);
        assert_eq!(package.contract.code_type, *b"RV64");
        assert!(resources.iter().any(|r| r.kind == *b"EABI" && r.id == 0));
        let scope = if artwork { ResourceScope::Application } else { ResourceScope::System };
        mac_system::testing::with_resource_forks(&system_resources, &file.resources, |services| {
            // The real service table remains pinned for the entire public SDK call.
            let system = unsafe { System::from_raw(services) }.unwrap();
            let icons = system.quickdraw().icons();
            let id = if artwork
                     {
                         icons.application()
                              .for_file(TypeCode::new(*b"STRP"), TypeCode::new(*b"INIT"))
                     }
                     else
                     {
                         icons.system().for_file(TypeCode::new(*b"MACS"), TypeCode::new(*b"INIT"))
                     }.unwrap();
            let mut surface = mac_abi::Surface { pixels: expected.as_mut_ptr(),
                                                 width:  width as u32,
                                                 height: height as u32,
                                                 origin: Point::default(),
                                                 clip:   Rect::new(0, 0, width as i16,
                                                                   height as i16) };
            // The connection exclusively borrows this retained pixel surface.
            let connection =
                unsafe { mac_traps::SurfaceConnection::new(&mut surface, services.trap) };
            SurfacePort::new(connection).plot_icon(scope, id, bounds, IconFlags::default())
                                        .unwrap();
        }).unwrap();
        if refused
        {
            // Native refusal styling is an explicit 28px X/halo contract. It is
            // not claimed as a failed original-Macintosh extension pixel match.
            for y in 2usize..30
            {
                for x in 2usize..30
                {
                    let distance = x.abs_diff(y).min(x.abs_diff(31 - y));
                    if distance <= 2
                    {
                        expected[(bounds.top as usize + y) * width + bounds.left as usize + x] =
                            if distance <= 1 { 0 } else { 0xffffff };
                    }
                }
            }
        }
        let changed = (bounds.top..bounds.bottom).flat_map(|y| {
                                                     (bounds.left..bounds.right).map(move |x| {
                                                                                    y as usize
                                                                                    * width
                                                                                    + x as usize
                                                                                })
                                                 })
                                                 .filter(|&at| actual[at] != before[at])
                                                 .count();
        assert!(changed > 20, "{name} has no visible real startup outcome");
        cells.push(bounds);
    }
    for (at, (&actual, &expected)) in actual.iter().zip(&expected).enumerate()
    {
        let (x, y) = (at % width, at / width);
        assert_eq!(actual, expected, "native startup pixel mismatch at {x},{y}");
        if !cells.iter().any(|bounds| {
                            x >= bounds.left as usize
                            && x < bounds.right as usize
                            && y >= bounds.top as usize
                            && y < bounds.bottom as usize
                        })
        {
            assert_eq!(actual, before[at], "unrelated boot pixel changed at {x},{y}");
        }
    }
    for folder in ["dist", "vm"]
    {
        for entry in fs::read_dir(output.join(folder)).unwrap()
        {
            let path = entry.unwrap().path();
            assert!(!matches!(path.extension().and_then(|value| value.to_str()),
                              Some("raw" | "qcow2" | "hfs" | "hfsb" | "img" | "bin")),
                    "private VM media retained after successful reap: {}",
                    path.display());
        }
    }
    fs::write(output.join("rust-acceptance.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "status":"passed", "scope":"native-startup-strip-only", "real_init_entries":3,
        "package_artwork":"public-SDK-matched", "fallback":"public-SDK-matched",
        "initialization_refusal":"real-entry-status-and-X-matched", "outside_cells":"unchanged",
        "desktop_after_refusal":"ready-and-physical-input-verified", "production_worker":"unqualified"
    })).unwrap()).unwrap();
}
