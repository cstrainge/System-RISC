use std::{fs,
          path::{Path, PathBuf},
          process::Command};
struct Scratch(PathBuf);
impl Drop for Scratch
{
    fn drop(&mut self)
    {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn run(arguments: &[&str]) -> String
{
    let output = Command::new(env!("CARGO_BIN_EXE_hfs-b")).args(arguments).output().unwrap();
    assert!(output.status.success(),
            "{:?}: {}",
            arguments,
            String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap()
}
fn path(p: &Path) -> &str
{
    p.to_str().unwrap()
}
#[test]
fn path_based_cli_copies_collections_both_ways_and_imports_named_macbinary()
{
    let scratch =
        Scratch(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build/system7/\
                                                                hfs-b-tool-tests")
                                                         .join(format!("cli-{}",
                                                                       std::process::id())));
    fs::create_dir_all(&scratch.0).unwrap();
    let source = scratch.0.join("Documents");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("Café.json"), b"{\"version\":1}").unwrap();
    let first = scratch.0.join("first.hfsb");
    let fresh = scratch.0.join("fresh.hfsb");
    let export = scratch.0.join("saved-documents");
    for image in [&first, &fresh]
    {
        run(&["create", path(image), "16777216", "Software"]);
    }
    run(&["copy-in", path(&first), path(&source), "/"]);
    let stat: serde_json::Value =
        serde_json::from_str(&run(&["stat", path(&first), "/documents/CAFÉ.JSON"])).unwrap();
    assert_eq!(stat["name"], "Café.json");
    assert_eq!(stat["data_length"], 13);
    run(&["copy-out", path(&first), "/Documents", path(&export)]);
    run(&["copy-in", path(&fresh), path(&export), "/"]);
    let extracted = scratch.0.join("restored.json");
    run(&["extract", path(&fresh), "/Documents/Café.json", path(&extracted)]);
    assert_eq!(fs::read(extracted).unwrap(), b"{\"version\":1}");
    run(&["mkdir", path(&fresh), "/", "Applications"]);
    let macbinary = scratch.0.join("Application.bin");
    fs::write(&macbinary,mac_formats::macbinary::encode(&mac_formats::macbinary::MacFile {
        name:b"Application".to_vec(),file_type:*b"APPL",creator:*b"TEST",flags:0x0804,
        created:3_875_000_000,modified:3_875_000_001,data:b"data fork".to_vec(),resources:b"resource fork".to_vec()
    }).unwrap()).unwrap();
    run(&["import", path(&fresh), "/Applications", path(&macbinary), "Copied Application 😀"]);
    let stat: serde_json::Value = serde_json::from_str(&run(&["stat",
                                                              path(&fresh),
                                                              "/Applications/Copied \
                                                               Application 😀"])).unwrap();
    assert_eq!(stat["name"], "Copied Application 😀");
    assert_eq!(stat["resource_length"], 13);
    let resources = scratch.0.join("resources");
    run(&["extract",
          path(&fresh),
          "/Applications/Copied Application 😀",
          path(&resources),
          "resource"]);
    assert_eq!(fs::read(resources).unwrap(), b"resource fork");
    run(&["check", path(&fresh)]);
}
