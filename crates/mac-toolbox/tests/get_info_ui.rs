//! Finder Info through actual public SDK windows/editors/files and physical input.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs,
          path::{Path, PathBuf},
          process::Command};

use mac_toolbox::{System,
                  files::{Fork, ROOT_ID}};
use serde_json::Value;

struct DiskSnapshots(PathBuf);
impl Drop for DiskSnapshots
{
    fn drop(&mut self)
    {
        for name in ["boot-1.hfs", "persisted.hfs"]
        {
            let _ = fs::remove_file(self.0.join(name));
        }
        let _ = fs::write(self.0.join("media-cleanup.json"),
                          b"{\"owned_media_removed_after_rust_checks\":true}\n");
    }
}
fn frame(path: &Path) -> Vec<u8>
{
    let bytes = fs::read(path).unwrap();
    let header = b"P6\n640 480\n255\n";
    assert!(bytes.starts_with(header));
    assert_eq!(bytes.len(), header.len() + 640 * 480 * 3);
    bytes[header.len()..].to_vec()
}
fn region(bytes: &[u8], bounds: [usize; 4]) -> Vec<u8>
{
    let [left, top, right, bottom] = bounds;
    (top..bottom).flat_map(|y| bytes[(y * 640 + left) * 3..(y * 640 + right) * 3].iter().copied())
                 .collect()
}
fn fields(line: &str) -> std::collections::HashMap<&str, &str>
{
    line.split(" name=")
        .next()
        .unwrap()
        .split_whitespace()
        .skip(1)
        .filter_map(|field| field.split_once('='))
        .collect()
}
#[test]
fn finder_info_resource_layouts_are_observed_through_the_public_resource_manager()
{
    let file = include_bytes!("../../../finder/assets/get-info/layout-17011.bin");
    let folder = include_bytes!("../../../finder/assets/get-info/layout-11261.bin");
    let resources=[(17011,file.as_slice()),(11261,folder.as_slice())].into_iter()
        .map(|(id,data)|mac_formats::resource::Resource{kind:*b"gInf",id,name:None,attributes:0,data:data.to_vec()}).collect::<Vec<_>>();
    let fork = mac_formats::resource::encode(&resources).unwrap();
    mac_system::testing::with_resource_forks(&fork, &fork, |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        for (id, height, comment_top) in [(17011, 261, 179), (11261, 217, 155)]
        {
            let resource = system.resources()
                                 .application()
                                 .get(mac_toolbox::ResourceKey { kind: *b"gInf",
                                                                 id })
                                 .unwrap();
            let mut bytes = [0; 104];
            assert_eq!(resource.read(0, &mut bytes).unwrap(), 104);
            resource.release().unwrap();
            assert_eq!(&bytes[..4], b"GIn1");
            let word = |offset| i16::from_be_bytes([bytes[offset], bytes[offset + 1]]);
            assert_eq!((word(4), word(6)), (242, height));
            let rect = |index: usize| {
                let offset = 8 + index * 8;
                [word(offset), word(offset + 2), word(offset + 4), word(offset + 6)]
            };
            // Original fvew local top/left/bottom/right rectangles, independently
            // corroborated by the original runtime WindowRecord/ControlRecords.
            assert_eq!(rect(0), [16, 71, 28, 234]);
            assert_eq!(rect(2), [45, 71, 57, 234]);
            assert_eq!(rect(3), [57, 71, 81, 234]);
            assert_eq!(rect(4), [81, 71, 117, 234]);
            assert_eq!(rect(5), [117, 71, 129, 234]);
            assert_eq!(rect(6), [129, 71, 141, 234]);
            assert_eq!(rect(8), [comment_top, 8, comment_top + 54, 234]);
            if id == 17011
            {
                assert_eq!(rect(10), [241, 6, 253, 135]);
                assert_eq!(rect(11), [241, 137, 253, 234]);
            }
            else
            {
                assert_eq!(rect(7), [0; 4]);
                assert_eq!(rect(10), [0; 4]);
                assert_eq!(rect(11), [0; 4]);
            }
        }
    }).unwrap();
}
#[test]
#[ignore = "requires current OS image, original Info references and private graphical QEMU"]
fn editable_info_windows_persist_through_real_filesystem_commits_and_cold_boot()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let windows = std::env::var_os("SYSTEM7_GET_INFO_WINDOWS").is_some();
    let output =
        root.join("results/system7").join(format!("get-info-ui-{}-{}",
                                                  if windows { "windows" } else { "linux" },
                                                  std::process::id()));
    let mut command = Command::new("python3");
    command.arg(root.join("System7/scripts/capture_system7_get_info.py"))
           .arg("--output")
           .arg(&output)
           .current_dir(&root);
    if windows
    {
        command.arg("--windows-headless");
    }
    let transport = command.output().unwrap();
    assert!(transport.status.success(),
            "Get Info transport failed:\n{}\n{}",
            String::from_utf8_lossy(&transport.stdout),
            String::from_utf8_lossy(&transport.stderr));
    let _snapshots = DiskSnapshots(output.clone());
    let report: Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["original_before"], report["original_after"]);
    assert_eq!(report["owned_vm_exited"], true);
    let boot1 = fs::read_to_string(output.join("boot-1.log")).unwrap();
    let boot2 = fs::read_to_string(output.join("boot-2.log")).unwrap();
    for serial in [&boot1, &boot2]
    {
        for failure in
            ["SYSTEM_PANIC:", "SYSTEM_TRAP:", "SYSTEM_ALLOCATION_FAILED:", "FINDER_ACTION_FAILED"]
        {
            assert!(!serial.contains(failure), "unexpected{failure}:\n{serial}");
        }
        assert!(serial.contains("TRAPS_READY: resident ecall dispatcher ABI=6"));
    }
    let observations = report["observations"].as_array().unwrap();
    for observation in observations
    {
        assert_eq!(observation["state"]["dropped"], "0", "events dropped during editing");
    }
    assert_eq!(observations.len(), 20);
    let observation = |name: &str| observations.iter().find(|row| row["name"] == name).unwrap();
    let image = |name: &str| frame(&output.join(observation(name)["frame"].as_str().unwrap()));
    let prefix = |name: &str| {
        let row = observation(name);
        let serial = if row["boot"] == 1 { &boot1 } else { &boot2 };
        &serial[..row["serial_length"].as_u64().unwrap() as usize]
    };
    let open = |name: &str| {
        prefix(name).lines().rev().find(|line| line.starts_with("FINDER_INFO_OPEN ")).unwrap()
    };
    let file_fields = fields(open("file-empty"));
    let file_cnid = file_fields["cnid"].parse::<u32>().unwrap();
    let folder_fields = fields(open("folder-empty"));
    let folder_cnid = folder_fields["cnid"].parse::<u32>().unwrap();
    assert_eq!(file_fields["folder"], "0");
    assert_eq!(folder_fields["folder"], "1");
    assert_ne!(file_cnid, folder_cnid);
    let reused = prefix("file-reused").lines()
                                      .rev()
                                      .find(|line| line.starts_with("FINDER_INFO_REUSED "))
                                      .unwrap();
    let reuse = fields(reused);
    assert_eq!(reuse["window"], fields(open("file-reopened"))["window"]);
    assert_eq!(reuse["cnid"], file_fields["cnid"]);
    assert_eq!(reuse["bounds"], "328,100,570,361");
    assert_eq!(prefix("file-reused").matches("FINDER_INFO_OPEN ").count(),
               2,
               "reuse allocated a duplicate window");
    assert_eq!(prefix("rename-conflict").matches("FINDER_INFO_ERROR ").count(), 1);
    assert_eq!(prefix("close-conflict-retained").matches("FINDER_INFO_ERROR ").count(), 2);
    assert_eq!(prefix("rename-conflict").matches("FINDER_INFO_CLOSED ").count(),
               prefix("close-conflict-retained").matches("FINDER_INFO_CLOSED ").count(),
               "failed save discarded the window/draft");
    assert_eq!(region(&image("file-reused"), [328, 81, 570, 100]),
               region(&image("close-conflict-retained"), [328, 81, 570, 100]),
               "failed rename changed committed title");
    assert_eq!(region(&image("file-reused"), [24, 122, 328, 134]),
               region(&image("close-conflict-retained"), [24, 122, 328, 134]),
               "failed rename changed the exposed browser caption");
    assert_ne!(region(&image("file-reused"), [328, 81, 570, 100]),
               region(&image("file-renamed"), [328, 81, 570, 100]),
               "successful rename did not update the Info title");
    assert!(region(&image("file-reused"), [24, 122, 328, 134])
            != region(&image("file-renamed"), [24, 122, 328, 134]),
            "successful rename did not redraw the exposed browser caption");
    let errors =
        boot1.lines().filter(|line| line.starts_with("FINDER_INFO_ERROR ")).collect::<Vec<_>>();
    assert_eq!(errors.len(), 2);
    assert!(errors.iter().all(|line| fields(line)["status"] == "-11"));
    assert!(!boot2.contains("FINDER_INFO_ERROR "));
    assert!(boot1.contains(&format!("FINDER_INFO_RENAMED volume=1 cnid={file_cnid} name=z \
                                     file\n")));
    assert!(boot1.contains(&format!("FINDER_INFO_RENAMED volume=1 cnid={folder_cnid} name=z \
                                     folder\n")));
    assert_eq!(fields(open("cold-file-reopened"))["cnid"], file_fields["cnid"]);
    assert_eq!(fields(open("cold-folder-reopened"))["cnid"], folder_fields["cnid"]);
    assert!(boot1.contains("items=3 logical=6912 allocated=16384\n"));
    for (name, expected_locked, expected_stationery) in [("file-locked", 1, 0),
                                                         ("file-locked-stationery-attempt", 1, 0),
                                                         ("file-stationery", 0, 1),
                                                         ("cold-file-locked", 1, 1)]
    {
        let last =
            prefix(name).lines().rev().find(|line| line.starts_with("FINDER_INFO_FLAGS ")).unwrap();
        let fields = fields(last);
        assert_eq!(fields["locked"].parse::<u8>().unwrap(), expected_locked);
        assert_eq!(fields["stationery"].parse::<u8>().unwrap(), expected_stationery);
    }
    let mut persisted = Vec::new();
    for (name, locked) in [("boot-1.hfs", false), ("persisted.hfs", true)]
    {
        let(snapshot,activity)=mac_system::testing::with_file_disk(&output.join(name),true,Default::default(),|services|{
            let system=unsafe{System::from_raw(services)}.unwrap();let files=system.files();
            let file=files.lookup(1,ROOT_ID,"0 Info Cases/z file").unwrap();let folder=files.lookup(1,ROOT_ID,"0 Info Cases/z folder").unwrap();
            assert_eq!((file.id,folder.id),(file_cnid,folder_cnid));assert_eq!(file.is_locked(),locked);assert!(file.is_stationery());
            assert_eq!(file.data_length,36);assert_eq!(file.resource_length,0);assert_eq!(file.created,3_875_209_985);assert_eq!(file.modified,file.created);
            let mut file_comment=String::new();files.comment(1,file.id,&mut file_comment).unwrap();assert_eq!(file_comment,"native comment\rsecond line","locked editing changed the comment");
            let mut folder_comment=String::new();files.comment(1,folder.id,&mut folder_comment).unwrap();assert_eq!(folder_comment,"folder comment\rsecond line");
            let mut opened=files.open(1,file.id,Fork::Data).unwrap();let mut bytes=[0;36];assert_eq!(opened.read(&mut bytes).unwrap(),36);assert_eq!(&bytes[..24],b"Native Get Info fixture\r");
            serde_json::json!({"file_cnid":file.id,"folder_cnid":folder.id,"locked":file.is_locked(),"stationery":file.is_stationery(),"file_comment":file_comment,"folder_comment":folder_comment,"creation":file.created,"modification":file.modified})
        }).unwrap();
        assert_eq!(activity.writes, 0);
        persisted.push(snapshot);
    }
    eprintln!("Get Info public-SDK cold-boot filesystem assertions passed");
    // Pixel assertions intentionally compare immutable UI fields, not different
    // file paths, date values, application kind strings or editable-name behavior.
    let mut compared = 0usize;
    for (actual, reference, folder, locked) in
        [("file-empty", "02-plain-file", false, false),
         ("file-locked", "05-lock-on", false, true),
         ("file-stationery", "16-stationery-enabled", false, false),
         ("folder-empty", "07-empty-folder", true, false),
         ("nested-totals", "08-nested-folder", true, false)]
    {
        let observation_name = actual;
        let actual = image(actual);
        let reference = frame(&output.join(format!("original-{reference}.ppm")));
        let mut bounds = vec![[8, 91, 77, 103],
                              [8, 103, 77, 115],
                              [8, 127, 77, 139],
                              [8, 163, 77, 175],
                              [8, 175, 77, 187]];
        if observation_name == "file-empty"
        {
            bounds.push([79, 62, 122, 75]);
        }
        if !folder
        {
            bounds.push([8, 187, 77, 199]);
            bounds.push([8, 213, 77, 225]);
            bounds.push([14, 287, 242, 299]);
        }
        else
        {
            bounds.push([8, 189, 77, 201]);
        }
        let top = if folder { 201 } else { 225 };
        let bottom = top + 54;
        bounds.extend([[16, top, 242, top + 1],
                       [16, bottom - 1, 242, bottom],
                       [16, top, 17, bottom],
                       [241, top, 242, bottom]]);
        for bounds in bounds
        {
            let expected = region(&reference, bounds);
            let actual = region(&actual, bounds);
            let different = actual.as_chunks::<3>()
                                  .0
                                  .iter()
                                  .zip(expected.as_chunks::<3>().0.iter())
                                  .filter(|(a, b)| a != b)
                                  .count();
            assert_eq!(different, 0,
                       "Info fixed UI pixels differ at{bounds:?}, folder={folder} locked={locked}");
            compared += expected.len() / 3;
        }
    }
    let acceptance = serde_json::json!({"status":"passed","physical_input":"passed","window_reuse_and_moved_position":"passed","rename_identity_and_browser_titles":"passed","save_failure_retains_window":"duplicate name failure; I/O rollback covered by backend hosttests","cold_boot_persistence":"passed","original_exact_fixed_ui_pixels":compared,"persisted":persisted,"capture":report});
    let bytes = serde_json::to_vec_pretty(&acceptance).unwrap();
    fs::write(output.join("rust-acceptance.json"), &bytes).unwrap();
    fs::write(root.join("results/system7").join(if windows
                                                {
                                                    "get-info-ui-windows-checks.json"
                                                }
                                                else
                                                {
                                                    "get-info-ui-checks.json"
                                                }),
              &bytes).unwrap();
}
