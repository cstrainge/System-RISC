//! ResEdit's real document/export code through public SDK and resident services.
#![cfg(not(target_arch = "riscv64"))]
extern crate alloc;
use std::{fs, path::PathBuf, process::Command};

use mac_formats::{macbinary::{self, MacFile},
                  resource::{self, Resource}};
use mac_toolbox::{ResourceKey, System};
#[path = "../../../apps/resedit/src/document.rs"]
mod document;
#[path = "../../../apps/resedit/src/preview.rs"]
mod preview;

fn tool(root: &std::path::Path, home: &std::path::Path, name: &str, args: &[&str])
{
    let out =
        Command::new(root.join(".toolchains/hfsutils/rootfs/usr/bin").join(name)).args(args)
                                                                                 .env("HOME", home)
                                                                                 .env("TZ", "UTC")
                                                                                 .output()
                                                                                 .unwrap();
    assert!(out.status.success(), "{name}: {}", String::from_utf8_lossy(&out.stderr));
}
#[test]
fn file_browser_and_lossless_export_preserve_all_resources_and_both_forks()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let dir = root.join(format!("results/system7/resedit-host-{}", std::process::id()));
    fs::create_dir_all(dir.join("hfs-state")).unwrap();
    let disk = dir.join("disk.hfs");
    fs::File::create(&disk).unwrap().set_len(8 * 1024 * 1024).unwrap();
    let mut expected = Vec::new();
    for (kind, id, name, attributes, data) in
        [(*b"CODE", i16::MIN, None, 0xff, vec![0x4e, 0x75, 0, 255]),
         (*b"RV64", 0, Some(b"Executable".to_vec()), 0, vec![0, 1, 2]),
         (*b"NABI", 0, None, 0, vec![255; 32]),
         (*b"RLOC", 0, None, 0, vec![17; 77]),
         (*b"vers",
          1,
          None,
          0,
          vec![0x12, 0x34, 0x80, 0, 0, 0, 5, b'1', b'2', b'.', b'3', b'4', 0]),
         (*b"STR ", -1, Some(Vec::new()), 0x28, vec![3, b'C', b'a', 0x8e]),
         (*b"STR#", 10, Some(vec![0x8e; 255]), 0, vec![0, 2, 0, 2, b'x', 0xff]),
         (*b"MENU",
          128,
          None,
          0,
          vec![0, 128, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3, 4, b'F', b'i', b'l', b'e', 4, b'Q', b'u',
               b'i', b't', 0, b'Q', 0, 1, 0]),
         ([0, 0x80, 0xff, b' '], i16::MAX, Some(b"\"\\\r\n".to_vec()), 0x80, (0..=255).collect()),
         (*b"ICON", 128, None, 0, vec![255; 128]),
         (*b"cicn", 128, None, 0, vec![0; 8]),
         (*b"DATA", 100, None, 0, vec![0xa5; 1024 * 1024 + 123])]
    {
        expected.push(Resource { kind,
                                 id,
                                 name,
                                 attributes,
                                 data });
    }
    let original = MacFile { name:      b"Sample".to_vec(),
                             file_type: *b"APPL",
                             creator:   *b"TEST",
                             flags:     0x2000,
                             created:   3_875_171_696,
                             modified:  3_875_171_699,
                             data:      (0..=255).collect(),
                             resources: resource::encode(&expected).unwrap() };
    let home = dir.join("hfs-state");
    tool(&root, &home, "hformat", &["-l", "ResEdit Test", disk.to_str().unwrap()]);
    tool(&root, &home, "hmount", &[disk.to_str().unwrap()]);
    for (name, resources) in
        [("Sample", original.resources.clone()), ("Empty", vec![]), ("Broken", vec![0; 16])]
    {
        let mut file = original.clone();
        file.name = name.as_bytes().to_vec();
        file.resources = resources;
        let path = dir.join(format!("{name}.bin"));
        fs::write(&path, macbinary::encode(&file).unwrap()).unwrap();
        tool(&root, &home, "hcopy", &["-m", path.to_str().unwrap(), ":"]);
    }
    tool(&root, &home, "humount", &[]);
    let status = document::ExportError::Service(mac_toolbox::Error(-123));
    assert!(matches!(status, document::ExportError::Service(mac_toolbox::Error(-123))));
    let mut hex_lines = Vec::new();
    preview::hex(&[0, 255, 65], 16, |line| hex_lines.push(line.to_owned()));
    assert!(hex_lines[0].starts_with("00000010 00 ff 41"));
    let before = fs::read(&disk).unwrap();
    let (_,activity)=mac_system::testing::with_file_disk(&disk,true,Default::default(),|services| {
        let system=unsafe {System::from_raw(services)}.unwrap();let files=system.files();let catalog=files.lookup(1,2,"Sample").unwrap();
        let a=system.resources().open_file(1,catalog.id).unwrap();let b=system.resources().open_file(1,catalog.id).unwrap();
        assert_eq!(a.type_count().unwrap(),12);
        let mut actual=Vec::new();
        for t in 0..a.type_count().unwrap() {let kind=a.type_at(t).unwrap();for i in 0..a.count(kind).unwrap() {
            let info=a.at(kind,i).unwrap();let mut data=vec![0;info.info.length];for (chunk_index,chunk) in data.chunks_mut(1024).enumerate() { assert_eq!(a.read(info.info.key,chunk_index*1024,chunk).unwrap(),chunk.len()); }
            actual.push(Resource {kind,id:info.info.key.id,name:info.has_name.then(||mac_formats::text::encode(info.info.name()).unwrap()),attributes:info.info.attributes as u8,data});
        }}
        let sort=|v:&mut Vec<Resource>|v.sort_by_key(|r|(r.kind,r.id));sort(&mut actual);sort(&mut expected);assert_eq!(actual,expected);
        assert!(a.named(*b"STR ","").unwrap().has_name);assert!(a.named(*b"CODE","").is_err());
        assert_eq!(a.get(ResourceKey {kind:*b"STR ",id:-1}).unwrap().info.attributes,0x28);
        let mut guard=[0x77;8];assert!(a.read(ResourceKey {kind:*b"CODE",id:i16::MIN},5,&mut guard).is_err());assert_eq!(guard,[0x77;8]);
        assert_eq!(a.read(ResourceKey {kind:*b"CODE",id:i16::MIN},4,&mut guard).unwrap(),0);
        let mut json=String::new();document::export(&system,&a,&catalog,&mut json).unwrap();fs::write(dir.join("resources.json"),&json).unwrap();
        let manifest=mac_build::Manifest::read(&dir.join("resources.json")).unwrap();let mut rebuilt=manifest.compile_resources(&dir.join("resources.json"),b"must not replace code").unwrap();sort(&mut rebuilt);assert_eq!(rebuilt,expected);
        mac_build::package::rebuild(&dir.join("resources.json"),&dir.join("rebuilt")).unwrap();let output=macbinary::decode(&fs::read(dir.join("rebuilt/Sample.bin")).unwrap()).unwrap();
        assert_eq!(output.data,original.data);assert_eq!(output.file_type,original.file_type);assert_eq!(output.creator,original.creator);assert_eq!(output.flags,original.flags);assert_eq!(output.created,original.created);assert_eq!(output.modified,original.modified);let mut decoded=resource::decode(&output.resources).unwrap();sort(&mut decoded);assert_eq!(decoded,expected);
        let mut archive:serde_json::Value=serde_json::from_slice(&fs::read(dir.join("resources.json")).unwrap()).unwrap();
        archive["name"]=serde_json::json!("A/B\\C");let strange=dir.join("strange-name.json");fs::write(&strange,serde_json::to_vec(&archive).unwrap()).unwrap();
        let rebuilt_dir=dir.join("strange-name");let report=mac_build::package::rebuild(&strange,&rebuilt_dir).unwrap();let host_path=PathBuf::from(report["file"].as_str().unwrap());assert_eq!(host_path.parent().unwrap(),rebuilt_dir);
        let rebuilt=macbinary::decode(&fs::read(host_path).unwrap()).unwrap();assert_eq!(rebuilt.name,b"A/B\\C");assert_eq!(rebuilt.data,original.data);let mut resources=resource::decode(&rebuilt.resources).unwrap();sort(&mut resources);assert_eq!(resources,expected);
        drop(a);assert_eq!(b.type_count().unwrap(),12);drop(b);
        for _ in 0..80 {drop(system.resources().open_file(1,catalog.id).unwrap());}
        let empty=files.lookup(1,2,"Empty").unwrap();assert_eq!(system.resources().open_file(1,empty.id).unwrap().type_count().unwrap(),0);
        let broken=files.lookup(1,2,"Broken").unwrap();assert!(system.resources().open_file(1,broken.id).is_err());
        // Failed opens also release their owned marks rather than exhaust slots.
        for _ in 0..80 {assert!(system.resources().open_file(1,broken.id).is_err());}
        let mut lines=Vec::new();assert!(preview::structured(*b"STR#",&[0,2,1,b'A',1,0x8e],|s|lines.push(s.to_owned())).unwrap());assert_eq!(lines,["1: A","2: é"]);
        assert!(preview::structured(*b"STR#",&[0,1,3,b'A'],|_|()).is_err());
        let mut menu=Vec::new();preview::structured(*b"MENU",&expected.iter().find(|r|r.kind==*b"MENU").unwrap().data,|s|menu.push(s.to_owned())).unwrap();assert!(menu.iter().any(|s|s.contains("Quit")));assert!(menu.iter().any(|s|s.contains("key 81")));
    }).unwrap();
    assert_eq!(activity.writes, 0);
    assert_eq!(before, fs::read(&disk).unwrap());
}

fn pixels(path: &std::path::Path) -> Vec<u8>
{
    let bytes = fs::read(path).unwrap();
    let header = b"P6\n640 480\n255\n";
    assert!(bytes.starts_with(header));
    bytes[header.len()..].to_vec()
}
fn crop(bytes: &[u8], left: usize, top: usize, size: usize) -> Vec<u8>
{
    (top..top + size).flat_map(|y| {
                         bytes[(y * 640 + left) * 3..(y * 640 + left + size) * 3].iter().copied()
                     })
                     .collect()
}
#[test]
#[ignore = "requires cargo xtask image and an independent graphical QEMU guest"]
fn installed_resedit_browses_live_apps_previews_pixels_and_rebuilds_export_losslessly()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output = root.join(format!("results/system7/resedit-ui-{}", std::process::id()));
    let capture = Command::new("python3").arg(root.join("System7/scripts/capture_resedit.py"))
                                         .arg("--output")
                                         .arg(&output)
                                         .output()
                                         .unwrap();
    assert!(capture.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&capture.stdout),
            String::from_utf8_lossy(&capture.stderr));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["base_before"], report["base_after"]);
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    for marker in ["RESEDIT_ERROR", "SYSTEM_PANIC:", "SYSTEM_TRAP:", "DISK_REQUEST_FAILED",
                   "DISK_READ_FAILED", "SYSTEM_ALLOCATION_FAILED:"]
    {
        assert!(!serial.contains(marker), "{marker}: {serial}");
    }
    assert_eq!(serial.lines()
                     .filter(|line| {
                         line.starts_with("PROCESS_LAUNCHED ")
                         && line.ends_with("name=QuickDraw Test")
                     })
                     .count(),
               1,
               "inspection must not launch a second app");
    assert!(serial.contains("RESEDIT_OPEN volume=1"));
    assert!(serial.contains("QD_DEMO_READY page=2 transfer\n"));
    assert!(serial.contains("PROCESS_EXIT app=3\n"));
    assert_eq!(fs::read(output.join("native-resources.json")).unwrap(),
               fs::read(output.join("resources.json")).unwrap(),
               "native Save must preserve the lossless archive exactly");
    assert!(serial.contains("STANDARD_FILE_BEGIN "));
    assert!(serial.lines().filter(|line| line.starts_with("RESEDIT_SAVED ")).count() >= 3);
    assert_eq!(fs::read(output.join("native-subfolder-resources.json")).unwrap(),
               fs::read(output.join("resources.json")).unwrap(),
               "New Folder save must preserve the archive");
    let source =
        serde_json::from_slice::<serde_json::Value>(include_bytes!("data/standard-file/\
                                                                    provenance.json")).unwrap();
    let system_fork = fs::read(root.join("build/system7/dist/System.rsrc")).unwrap();
    mac_system::testing::with_resource_forks(&system_fork,
                                             &resource::encode(&[]).unwrap(),
                                             |services| {
                                                 let system =
                                                     unsafe { System::from_raw(services) }.unwrap();
                                                 let width =
                                                     system.quickdraw()
                                                           .fonts()
                                                           .text_width(Default::default(),
                                                                       "exports")
                                                           .unwrap()
                                                     as usize;
                                                 let frame =
                                                     pixels(&output.join("system-new-folder.ppm"));
                                                 let caret_x = (640 - 204) / 2 + 12 + width;
                                                 assert!((134..148).all(|y| {
                                                                       frame[(y * 640 + caret_x) * 3
                                                                             ..(y * 640 + caret_x)
                                                                               * 3
                                                                               + 3]
                                                                       == [0; 3]
                                                                   }),
                                                         "filename caret must follow the \
                                                          complete text using resident font \
                                                          advances");
                                                 for row in source["resources"].as_array().unwrap()
                                                 {
                                                     let kind = row["type"].as_str()
                                                                           .unwrap()
                                                                           .as_bytes()
                                                                           .try_into()
                                                                           .unwrap();
                                                     let id = row["id"].as_i64().unwrap() as i16;
                                                     let entry = system.resources()
                                                                       .system()
                                                                       .get(ResourceKey { kind,
                                                                                          id })
                                                                       .unwrap();
                                                     let mut bytes = vec![0; entry.size()];
                                                     entry.read(0, &mut bytes).unwrap();
                                                     entry.release().unwrap();
                                                     assert_eq!(mac_build::package::sha256(&bytes),
                                                                row["sha256"].as_str().unwrap(),
                                                                "original Standard File resource \
                                                                 changed");
                                                 }
                                             }).unwrap();
    let saved = Command::new("python3").arg(root.join("System7/scripts/export_resedit_json.py"))
                                       .arg("--serial")
                                       .arg(output.join("vm/serial.log"))
                                       .arg("--output")
                                       .arg(output.join("export-via-tool.json"))
                                       .output()
                                       .unwrap();
    assert!(saved.status.success(), "{}", String::from_utf8_lossy(&saved.stderr));
    assert_eq!(fs::read(output.join("resources.json")).unwrap(),
               fs::read(output.join("export-via-tool.json")).unwrap());
    let truncated = serial.lines()
                          .filter(|line| !line.starts_with("RESEDIT_JSON_END "))
                          .collect::<Vec<_>>()
                          .join("\n");
    fs::write(output.join("incomplete.log"), truncated).unwrap();
    let rejected = Command::new("python3").arg(root.join("System7/scripts/export_resedit_json.py"))
                                          .arg("--serial")
                                          .arg(output.join("incomplete.log"))
                                          .arg("--output")
                                          .arg(output.join("must-not-exist.json"))
                                          .output()
                                          .unwrap();
    assert!(!rejected.status.success());
    assert!(!output.join("must-not-exist.json").exists());
    let original=macbinary::decode(&fs::read(root.join("build/system7/dist/QuickDraw Test.bin")).unwrap()).unwrap();
    mac_build::package::rebuild(&output.join("resources.json"), &output.join("rebuilt")).unwrap();
    let rebuilt =
        macbinary::decode(&fs::read(output.join("rebuilt/QuickDraw Test.bin")).unwrap()).unwrap();
    assert_eq!(original.data, rebuilt.data);
    assert_eq!(resource::decode(&original.resources).unwrap(),
               resource::decode(&rebuilt.resources).unwrap());
    assert_eq!(original.name, rebuilt.name);
    assert_eq!(original.file_type, rebuilt.file_type);
    assert_eq!(original.creator, rebuilt.creator);
    assert_eq!(original.flags, rebuilt.flags);
    mac_build::package::inspect(&fs::read(output.join("rebuilt/QuickDraw Test.bin")).unwrap()).unwrap();
    let frames = report["frames"].as_array().unwrap();
    for (package, kind) in
        [("QuickDraw Test", *b"ICN#"), ("System", *b"ICON"), ("System", *b"cicn")]
    {
        let name = format!("{package}-{}",
                           kind.iter().map(|byte| format!("{byte:02x}")).collect::<String>());
        let row = frames.iter().find(|row| row["name"] == name).unwrap();
        let image = pixels(&output.join(row["frame"].as_str().unwrap()));
        assert_eq!(&image[(80 * 640 + 500) * 3..(80 * 640 + 500) * 3 + 3],
                   &[255; 3],
                   "browser background must be white");
        let bytes = fs::read(root.join(format!("build/system7/dist/{package}.rsrc"))).unwrap();
        let entries = resource::decode(&bytes).unwrap();
        let item = entries.iter().find(|item| item.kind == kind).unwrap();
        let mut expected = vec![0xffffffu32; 128 * 128];
        mac_system::testing::with_resource_forks(&resource::encode(&[]).unwrap(),&resource::encode(&[]).unwrap(),|services| {
            let system=unsafe{System::from_raw(services)}.unwrap();
            let mut surface=mac_abi::Surface {pixels:expected.as_mut_ptr(),width:128,height:128,origin:Default::default(),clip:mac_abi::Rect::new(0,0,128,128)};
            let mut call=mac_abi::quickdraw::IconDataCall {kind,surface:&mut surface,destination:mac_abi::Rect::new(0,0,128,128),..Default::default()};
            system.client().icon_data(&mut call,&item.data).unwrap();
            let before=expected.clone();assert!(system.client().icon_data(&mut call,&[0;3]).is_err());assert_eq!(before,expected,"malformed icon must not partially paint");
        }).unwrap();
        let expected: Vec<u8> =
            expected.into_iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, p as u8]).collect();
        assert!(crop(&image, 310, 174, 128) == expected,
                "{package} {kind:?}: raw icon preview differs from the public SDK surface \
                 rendering");
    }
    let observation = |name: &str| frames.iter().find(|row| row["name"] == name).unwrap();
    for name in ["open-held", "open-left", "open-cancelled"]
    {
        assert!(!observation(name)["log"].as_str().unwrap().contains("RESEDIT_OPEN "),
                "Open acted before accepted release");
    }
    assert_ne!(pixels(&output.join(observation("open-held")["frame"].as_str().unwrap())),
               pixels(&output.join(observation("open-left")["frame"].as_str().unwrap())),
               "CDEF press must track pointer departure");
    let icon_provenance:serde_json::Value=serde_json::from_slice(&fs::read(root.join("System7/apps/resedit/assets/provenance.json")).unwrap()).unwrap();
    let fork = fs::read(root.join("build/system7/dist/ResEdit.rsrc")).unwrap();
    mac_system::testing::with_resource_forks(&resource::encode(&[]).unwrap(), &fork, |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        for item in icon_provenance["resources"].as_array().unwrap()
        {
            let kind = mac_formats::text::encode(item["type"].as_str().unwrap()).unwrap()
                                                                                .try_into()
                                                                                .unwrap();
            let id = item["id"].as_i64().unwrap() as i16;
            let resource = system.resources()
                                 .application()
                                 .get(ResourceKey { kind,
                                                    id })
                                 .unwrap();
            let mut data = vec![0; resource.size()];
            assert_eq!(resource.read(0, &mut data).unwrap(), data.len());
            resource.release().unwrap();
            assert_eq!(mac_build::package::sha256(&data),
                       item["sha256"].as_str().unwrap(),
                       "original ResEdit icon/bundle bytes changed");
        }
    }).unwrap();
    for kind in [*b"MENU", *b"STR ", *b"STR#", *b"vers", *b"RV64"]
    {
        let marker = format!("RESEDIT_RESOURCE kind={}", u32::from_be_bytes(kind));
        assert!(serial.contains(&marker), "missing viewer {kind:?}");
    }
    fs::write(root.join("results/system7/resedit-checks.json"),serde_json::to_vec_pretty(&serde_json::json!({"passed":true,"running_app":true,"lossless_export":true,"icon_pixels":3*128*128,"capture":report})).unwrap()).unwrap();
}
