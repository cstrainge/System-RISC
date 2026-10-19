//! ResEdit's real document/export code through public SDK and resident services.
#![cfg(not(target_arch = "riscv64"))]
extern crate alloc;
use std::{fs, path::PathBuf, process::Command};

use mac_formats::{macbinary::{self, MacFile},
                  resource::{self, Resource}};
use mac_toolbox::{ResourceKey, System};
#[path = "../../../apps/resedit/src/code.rs"]
mod code;
#[path = "../../../apps/resedit/src/view.rs"]
mod view;
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
         (*b"RV64", 0, Some(b"Executable".to_vec()), 0, sample_elf()),
         (*b"NABI",
          0,
          None,
          0,
          mac_formats::native::Descriptor { role:        mac_formats::native::Role::Application,
                                            abi_version: mac_formats::native::ABI_VERSION,
                                            load_base:   0x86800000,
                                            load_limit:  0x87000000 }.encode()
                                                                     .unwrap()
                                                                     .to_vec()),
         (*b"RLOC",
          0,
          None,
          0,
          vec![b'R', b'L', b'C', b'1', 1, 0, 0, 0, 0x20, 0, 0, 0, 8, 0, 0, 0]),
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
        // Decode through the same public, owner-checked resource reader as the
        // installed app, including checkpoints and reads crossing cache edges.
        let key=ResourceKey {kind:*b"RV64",id:0};
        let size=a.get(key).unwrap().info.length;
        let index=code::Index::build(size,|at,out|a.read(key,at,out)).unwrap();
        assert_eq!(index.entry,0x86800000);assert_eq!(index.rows,40002);
        for start in [0,1,1364,1365,16383,32767,39996,40000,40001,40002] {
            let mut actual=Vec::new();index.lines(start,8,|at,out|a.read(key,at,out),|line|actual.push(line.to_owned())).unwrap();
            let expected:Vec<_>=(start..(start+8).min(40002)).map(|row| {
                if row==40001 {"90000000 ecall".to_owned()}
                else if row==40000 {"8681d4c0 .byte ff".to_owned()}
                else {format!("{:08x} {}",0x86800000u64+u64::from(row/2)*6+if row%2==0 {0}else{4},if row%2==0 {"li a0,1"}else{"ret"})}
            }).collect();assert_eq!(actual,expected,"row {start}");
        }
        let mut loader=Vec::new();code::loader(&a,index.entry,|line|loader.push(line.to_owned()));assert_eq!(loader.len(),code::HEADER_ROWS as usize);assert!(loader[0].contains("RV64"));assert_eq!(loader[2],"NABI: format 1 role 2 ABI 6");assert_eq!(loader[3],"Load base: 0x86800000");assert_eq!(loader[4],"Load limit: 0x87000000");assert!(loader[5].starts_with("RLOC: 1 patches"));
        let mut records=Vec::new();code::relocations(a.get(ResourceKey{kind:*b"RLOC",id:0}).unwrap().info.length,1,1,|at,out|a.read(ResourceKey{kind:*b"RLOC",id:0},at,out),|line|records.push(line.to_owned())).unwrap();assert_eq!(records,["+00000020  64-bit pointer"]);
        assert!(code::Index::build(3,|at,out|a.read(ResourceKey{kind:*b"CODE",id:i16::MIN},at,out)).is_err());
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
    for marker in ["RESEDIT_ERROR", "RESEDIT_SAVE_ERROR", "SYSTEM_PANIC:", "SYSTEM_TRAP:",
                   "DISK_REQUEST_FAILED", "DISK_READ_FAILED", "SYSTEM_ALLOCATION_FAILED:"]
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
        assert!(crop(&image, 310, 139, 128) == expected,
                "{package} {kind:?}: raw icon preview differs from the public SDK surface \
                 rendering");
    }
    mac_system::testing::with_resource_forks(&system_fork,&resource::encode(&[]).unwrap(),|services|{
        let system=unsafe{System::from_raw(services)}.unwrap();let font=mac_toolbox::quickdraw::fonts::FontSelection{family:4,size:9,face:0};
        assert_eq!(system.quickdraw().fonts().text_width(font,"iiii 0000").unwrap(),system.quickdraw().fonts().text_width(font,"WWWW 1111").unwrap(),"Monaco must provide fixed-width columns");
    }).unwrap();
    // Compare actual hex rows with resident Monaco rasterization. This checks
    // the selected font and complete fixed-pitch address/byte/ASCII alignment.
    for package in ["QuickDraw Test", "System"]
    {
        let name = format!("{package}-hex");
        let row = frames.iter().find(|r| r["name"] == name).unwrap();
        let image = pixels(&output.join(row["frame"].as_str().unwrap()));
        let fork = fs::read(root.join(format!("build/system7/dist/{package}.rsrc"))).unwrap();
        let entries = resource::decode(&fork).unwrap();
        let code = entries.iter().find(|r| r.kind == *b"RV64" && r.id == 0).unwrap();
        assert!(code.data.starts_with(b"\x7fELF"), "native executable must be visible as RV64 0");
        let mut expected = vec![0xffffffu32; 288 * 224];
        mac_system::testing::with_resource_forks(&system_fork,&resource::encode(&[]).unwrap(),|services|{
            let mut surface=mac_abi::Surface{pixels:expected.as_mut_ptr(),width:288,height:224,origin:Default::default(),clip:mac_abi::Rect::new(0,0,288,224)};
            let mut connection=unsafe{mac_traps::SurfaceConnection::new(&mut surface,services.trap)};
            let mut line=0;
            preview::hex(&code.data[..128],0,|text|{
                connection.font_draw(mac_toolbox::quickdraw::fonts::FontSelection{family:4,size:9,face:0},mac_toolbox::Point{h:0,v:12+line*14},text,0).unwrap();line+=1;
            });
        }).unwrap();
        let actual: Vec<_> = (129..353).flat_map(|y| {
                                           image[(y * 640 + 295) * 3..(y * 640 + 583) * 3].iter()
                                                                                          .copied()
                                       })
                                       .collect();
        let expected: Vec<_> =
            expected.into_iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, p as u8]).collect();
        assert!(actual == expected,
                "{package} hex preview must use Monaco 9, including fixed-pitch columns");
    }
    let observation = |name: &str| frames.iter().find(|row| row["name"] == name).unwrap();
    // Radio state/animation must be actual CDEF0 output, including press and
    // cancellation. The standard CDEF itself has original-System pixel tests.
    mac_system::testing::with_resource_forks(&system_fork, &resource::encode(&[]).unwrap(), |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        for package in ["QuickDraw Test", "System"]
        {
            for (suffix, hex, pressed) in [("hex-radio-held",false,true),
                                            ("hex-radio-left",false,false),
                                            ("hex-radio-cancelled",false,false),
                                            ("hex",true,false),
                                            ("disassembly-restored",false,false)]
            {
                let name = format!("{package}-{suffix}");
                let image = pixels(&output.join(observation(&name)["frame"].as_str().unwrap()));
                for (is_hex, left, width, title) in [(true,295,104,"Hex Dump"),(false,407,203,"Disassembly")]
                {
                    let mut expected = vec![0xffffff; width*21];
                    let mut state = mac_toolbox::controls::StandardState::new(mac_toolbox::Rect::new(0,0,width as i16,21),title);
                    state.value = i16::from(hex==is_hex);
                    state.highlighted = u32::from(is_hex && pressed);
                    system.controls().draw_standard_pixels(&mut expected,width,21,mac_toolbox::controls::StandardKind::Radio,state).unwrap();
                    // Held pointer stays in the bottom-right blank margin;
                    // retain the entire CDEF glyph and caption comparison.
                    let compared_width=if is_hex && pressed {width-8}else{width};
                    for y in 0..21 {for x in 0..compared_width {
                        let pixel=expected[y*width+x];
                        assert_eq!(&image[((y+50)*640+left+x)*3..((y+50)*640+left+x)*3+3],&[(pixel>>16) as u8,(pixel>>8) as u8,pixel as u8],"{name} {title} at {x},{y}");
                    }}
                }
            }
            let before=pixels(&output.join(observation(&format!("{package}-hex"))["frame"].as_str().unwrap()));
            let after=pixels(&output.join(observation(&format!("{package}-hex-down"))["frame"].as_str().unwrap()));
            for y in 48..425 {
                assert_eq!(&before[(y*640+13)*3..(y*640+291)*3],&after[(y*640+13)*3..(y*640+291)*3],"{package}: scrolling preview changed navigation panes");
            }
            for y in 130..410 {
                assert_eq!(&before[((y+14)*640+292)*3..((y+14)*640+596)*3],&after[(y*640+292)*3..(y*640+596)*3],"{package}: cached rows did not shift exactly one line");
            }
        }
    }).unwrap();
    let mut timings = Vec::new();
    for package in ["QuickDraw Test", "System"]
    {
        let start = observation(&format!("{package}-hex-down"))["log"].as_str().unwrap().len();
        let end = observation(&format!("{package}-hex-key-scroll"))["log"].as_str().unwrap();
        let samples: Vec<u64> = end[start..].lines()
                                            .filter_map(|line| {
                                                line.strip_prefix("RESEDIT_RENDER pane=2 ns=")
                                                    .map(|v| v.parse().unwrap())
                                            })
                                            .collect();
        assert_eq!(samples.len(), 8, "{package}: missing one-row render measurements");
        assert_eq!(end[start..].matches("RESEDIT_RENDER pane=all").count(),
                   0,
                   "key scrolling must not redraw the browser");
        timings.extend(samples);
    }
    timings.sort_unstable();
    let median_ns = timings[timings.len() / 2];
    let worst_ns = *timings.last().unwrap();
    assert!(median_ns < 50_000_000,
            "one-row fill and commit median is {}ms",
            median_ns / 1_000_000);
    mac_system::testing::with_resource_forks(&system_fork,&resource::encode(&[]).unwrap(),|services|{
        let system=unsafe{System::from_raw(services)}.unwrap();let font=mac_toolbox::quickdraw::fonts::FontSelection::default();
        for (name,title) in [("running-app-open","QuickDraw Test"),("System-49434f4e","System")] {
            let width=system.quickdraw().fonts().text_width(font,title).unwrap() as usize;
            let metrics=system.quickdraw().fonts().metrics(font).unwrap();
            let mut expected=vec![0xccccccu32;width*16];
            let mut surface=mac_abi::Surface{pixels:expected.as_mut_ptr(),width:width as u32,height:16,origin:Default::default(),clip:mac_abi::Rect::new(0,0,width as i16,16)};
            let mut connection=unsafe{mac_traps::SurfaceConnection::new(&mut surface,services.trap)};
            connection.font_draw(font,mac_toolbox::Point{h:0,v:metrics.ascent+1},title,0).unwrap();
            let actual=pixels(&output.join(observation(name)["frame"].as_str().unwrap()));let x=12+(616-width)/2;
            // Compare the glyph mask only, independent of the WDEF fill colors.
            for y in 0..16 {for col in 0..width {assert_eq!(actual[((30+y)*640+x+col)*3..((30+y)*640+x+col)*3+3]==[0;3],expected[y*width+col]==0,"filename title {title} at {col},{y}");}}
        }
    }).unwrap();
    // No window-level bottom scrollbar: the WDEF may draw only the size
    // handle, leaving the status text and the rest of the footer unobstructed.
    for (name, width, height) in [("System-disassembly-restored", 616, 412),
                                  ("resized", 600, 400),
                                  ("resize-restored", 616, 412)]
    {
        let image = pixels(&output.join(observation(name)["frame"].as_str().unwrap()));
        let y = 48 + height - 15;
        for x in 12 + 100..12 + width - 16
        {
            assert_eq!(&image[(y * 640 + x) * 3..(y * 640 + x) * 3 + 3],
                       &[255; 3],
                       "{name}: stray horizontal grow separator");
        }
        // The classic size handle must remain visible and usable.
        assert!((48 + height - 15..48 + height).any(|row| {
                                                   (12 + width - 15..12 + width).any(|col| {
                                                                                    image[(row
                                                                                           * 640
                                                                                           + col)
                                                                                          * 3
                                                                                          ..(row
                                                                                             * 640
                                                                                             + col)
                                                                                            * 3
                                                                                            + 3]
                                                                                    == [0; 3]
                                                                                })
                                               }),
                "{name}: size handle disappeared");
    }
    let viewport = |name: &str, key: &str| -> u32 {
        observation(name)["log"].as_str()
                                .unwrap()
                                .lines()
                                .rev()
                                .find(|line| line.starts_with("RESEDIT_VIEW "))
                                .unwrap()
                                .split_whitespace()
                                .find_map(|part| part.strip_prefix(&format!("{key}=")))
                                .unwrap()
                                .parse()
                                .unwrap()
    };
    assert!(viewport("System-57444546", "types_top") > 0,
            "WDEF must be reachable through the types scrollbar");
    assert_eq!(viewport("System-resources-down", "resources_top"), 1);
    assert_eq!(viewport("resized", "types_rows"), 21);
    assert_eq!(viewport("resized", "preview_rows"), 20);
    assert_eq!(viewport("resize-restored", "types_rows"), 22);
    assert_eq!(viewport("resize-restored", "preview_rows"), 21);
    for package in ["QuickDraw Test", "System"]
    {
        for suffix in ["hex-end", "disassembly-end"]
        {
            let name = format!("{package}-{suffix}");
            assert!(viewport(&name, "preview_top") > 0);
            assert_eq!(viewport(&name, "preview_top"), viewport(&name, "preview_max"), "{name}");
        }
        assert_eq!(viewport(&format!("{package}-hex-down"), "preview_top"), 1);
        let name = format!("{package}-disassembly-restored");
        assert_eq!(viewport(&name, "preview_top"), 0);
        assert!(observation(&name)["log"].as_str().unwrap().contains("mode=disassembly"));
        // Independently locate executable segments using the loader's parser;
        // compare the visible instruction rows with resident Monaco rendering.
        let fork = resource::decode(&fs::read(root.join(format!("build/system7/dist/{package}.\
                                                                 rsrc"))).unwrap()).unwrap();
        let bytes = &fork.iter().find(|r| r.kind == *b"RV64" && r.id == 0).unwrap().data;
        let elf = mac_formats::elf::decode(bytes, 0x80000000, 0x90000000).unwrap();
        let segment = elf.segments.iter().find(|s| s.flags & 1 != 0 && s.file_size != 0).unwrap();
        let instructions:Vec<_>=disc_v::disassemble(disc_v::Isa::Rv64,segment.address,&bytes[segment.offset as usize..(segment.offset+segment.file_size) as usize]).take(8).map(|ins|format!("{:08x} {ins}",ins.pc())).collect();
        let mut expected = vec![0xffffffu32; 288 * 112];
        mac_system::testing::with_resource_forks(&system_fork,&resource::encode(&[]).unwrap(),|services|{
            let mut surface=mac_abi::Surface{pixels:expected.as_mut_ptr(),width:288,height:112,origin:Default::default(),clip:mac_abi::Rect::new(0,0,288,112)};
            let mut connection=unsafe{mac_traps::SurfaceConnection::new(&mut surface,services.trap)};
            for (row,line) in instructions.iter().enumerate(){connection.font_draw(mac_toolbox::quickdraw::fonts::FontSelection{family:4,size:9,face:0},mac_toolbox::Point{h:0,v:12+row as i16*14},line,0).unwrap();}
        }).unwrap();
        let image = pixels(&output.join(observation(&name)["frame"].as_str().unwrap()));
        let actual: Vec<_> = (227..339).flat_map(|y| {
                                           image[(y * 640 + 295) * 3..(y * 640 + 583) * 3].iter()
                                                                                          .copied()
                                       })
                                       .collect();
        let expected: Vec<_> =
            expected.into_iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, p as u8]).collect();
        assert!(actual == expected, "{package} native instructions must use Monaco");
    }
    let icon_provenance:serde_json::Value=serde_json::from_slice(&fs::read(root.join("System7/apps/resedit/assets/provenance.json")).unwrap()).unwrap();
    let fork = fs::read(root.join("build/system7/dist/ResEdit.rsrc")).unwrap();
    mac_system::testing::with_resource_forks(&resource::encode(&[]).unwrap(), &fork, |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        assert!(system.resources()
                      .application()
                      .get(ResourceKey { kind: *b"MENU",
                                         id:   129 })
                      .is_err(),
                "viewer mode belongs to code-view radio buttons");
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
    fs::write(root.join("results/system7/resedit-checks.json"),serde_json::to_vec_pretty(&serde_json::json!({"passed":true,"running_app":true,"lossless_export":true,"icon_pixels":3*128*128,"radio_pixels":"CDEF0 press leave cancel select","scroll":{"samples":timings.len(),"median_ns":median_ns,"worst_ns":worst_ns,"scope":"one-row key fill plus resident commit"},"capture":report})).unwrap()).unwrap();
}

// Three load segments: mixed 32/16-bit instructions, non-code bytes, then
// another executable segment at a discontinuous linked address.
fn sample_elf() -> Vec<u8>
{
    let mut bytes = vec![0; 512];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
    bytes[18..20].copy_from_slice(&243u16.to_le_bytes());
    bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
    bytes[24..32].copy_from_slice(&0x86800000u64.to_le_bytes());
    bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
    bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
    bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
    bytes[56..58].copy_from_slice(&3u16.to_le_bytes());
    for _ in 0..20000
    {
        bytes.extend_from_slice(&[0x13, 0x05, 0x10, 0, 0x82, 0x80]);
    }
    bytes.push(0xff);
    let second = bytes.len();
    bytes.extend_from_slice(&[0xff; 16]);
    let third = bytes.len();
    bytes.extend_from_slice(&[0x73, 0, 0, 0]);
    for (i, (offset, address, size, flags)) in [(512, 0x86800000u64, 120001, 5),
                                                (second, 0x88000000, 16, 6),
                                                (third, 0x90000000, 4, 5)].into_iter()
                                                                          .enumerate()
    {
        let ph = &mut bytes[64 + i * 56..120 + i * 56];
        ph[..4].copy_from_slice(&1u32.to_le_bytes());
        ph[4..8].copy_from_slice(&u32::to_le_bytes(flags));
        ph[8..16].copy_from_slice(&(offset as u64).to_le_bytes());
        ph[16..24].copy_from_slice(&address.to_le_bytes());
        ph[32..40].copy_from_slice(&u64::to_le_bytes(size));
        ph[40..48].copy_from_slice(&u64::to_le_bytes(size));
    }
    bytes
}
#[test]
fn code_view_rejects_bad_elf_and_scrollbars_cover_large_resources()
{
    let good = sample_elf();
    for length in [0, 1, 16, 63, 64, 119, 511, good.len() - 1]
    {
        assert!(code::Index::build(length, |at, out| {
                    let source = good.get(at..).unwrap_or(&[]);
                    let n = source.len().min(out.len());
                    out[..n].copy_from_slice(&source[..n]);
                    Ok(n)
                }).is_err());
    }
    for at in [0, 4, 5, 16, 18, 20, 52, 54, 57, 96]
    {
        let mut bad = good.clone();
        bad[at] = 0xff;
        assert!(code::Index::build(bad.len(), |at, out| {
                    let n = out.len().min(bad.len().saturating_sub(at));
                    out[..n].copy_from_slice(&bad[at..at + n]);
                    Ok(n)
                }).is_err(),
                "offset {at}");
    }
    let layout = view::Layout { width:  616,
                                height: 412 };
    for pane in [view::Pane::Types, view::Pane::Resources, view::Pane::Preview]
    {
        assert!(layout.baseline(pane, 0) > layout.bounds(pane).top);
        let state = layout.scrollbar(pane);
        assert!(layout.row_at(pane,
                              mac_toolbox::Point { h: state.left,
                                                   v: state.top })
                      .is_none());
    }
    let mut relocation = b"RLC1".to_vec();
    relocation.extend_from_slice(&1u32.to_le_bytes());
    relocation.extend_from_slice(&[0x20, 0, 0, 0, 8, 0, 0, 0]);
    let mut lines = Vec::new();
    code::relocations(relocation.len(),
                      0,
                      10,
                      |at, out| {
                          let n = out.len().min(relocation.len() - at);
                          out[..n].copy_from_slice(&relocation[at..at + n]);
                          Ok(n)
                      },
                      |line| lines.push(line.to_owned())).unwrap();
    assert_eq!(lines, ["RLOC: 1 absolute pointer patches", "+00000020  64-bit pointer"]);
}
