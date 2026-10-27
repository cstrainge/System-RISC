//! Real Cargo/cache acceptance; the ordinary unit suite needs no toolchain/VM.
//! Set SYSTEM7_FIXTURE_CACHE_CHECK_ROOT to a separate private build directory.
use std::{fs,
          path::{Path, PathBuf},
          process::{Command, Output},
          time::Instant};

use mac_formats::{macbinary::{self, MacFile},
                  resource::{self, Resource}};
use serde_json::{Value, json};

const TRANSPORT: &str = r#"
import sys
import subprocess
sys.path.insert(0, sys.argv[1])
import system7_fixture_build as fixture
mode = sys.argv[3]
env = (fixture.host_env if mode == 'host' else fixture.guest_env)(sys.argv[2])
if mode == 'guest-cache-only':
    env.pop('SYSTEM7_CARGO_ARTIFACT_DIR', None)
raise SystemExit(subprocess.call(sys.argv[4:], env=env))
"#;

fn run(repo: &Path, root: &Path, target: &Path, mode: &str, arguments: &[&std::ffi::OsStr],
       label: &str, report: &mut Vec<Value>)
       -> Output
{
    let start = Instant::now();
    let output = Command::new(std::env::var_os("SYSTEM7_PYTHON").unwrap_or_else(|| "python3".into()))
        .arg("-c").arg(TRANSPORT).arg(repo.join("scripts")).arg(target)
        .arg(mode).arg("cargo").args(arguments)
        .env("SYSTEM7_BUILD_DIR", root).current_dir(repo.parent().unwrap())
        .output().unwrap();
    fs::write(root.join(format!("{label}.stdout")), &output.stdout).unwrap();
    fs::write(root.join(format!("{label}.stderr")), &output.stderr).unwrap();
    report.push(json!({"stage":label,"seconds":start.elapsed().as_secs_f64(),"success":output.status.success()}));
    assert!(output.status.success(),
            "{label} failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr));
    output
}
fn build(repo: &Path, root: &Path, fixture: &str, binary: &str, mode: &str,
         report: &mut Vec<Value>)
         -> Output
{
    let target = root.join("fixtures").join(fixture);
    let manifest = repo.join("crates/mac-toolbox/tests/fixtures").join(fixture).join("Cargo.toml");
    let mut args = vec!["build".as_ref(),
                        "--locked".as_ref(),
                        "--release".as_ref(),
                        "--target".as_ref(),
                        "riscv64imac-unknown-none-elf".as_ref(),
                        "--manifest-path".as_ref(),
                        manifest.as_os_str(),
                        "--bin".as_ref(),
                        binary.as_ref(),
                        "--target-dir".as_ref(),
                        target.as_os_str(),
                        "--message-format=json".as_ref(),
                        "--verbose".as_ref()];
    if fixture == "definition-panic"
    {
        args.extend([std::ffi::OsStr::new("--features"), std::ffi::OsStr::new("guest")]);
    }
    run(repo, root, &target, mode, &args, &format!("{fixture}-build-{}", report.len()), report)
}
fn package(repo: &Path, root: &Path, fixture: &str, binary: &str, report: &mut Vec<Value>)
           -> PathBuf
{
    let target = root.join("fixtures").join(fixture);
    let manifest = repo.join("crates/mac-toolbox/tests/fixtures").join(fixture).join("Cargo.toml");
    let elf = target.join("riscv64imac-unknown-none-elf/release").join(binary);
    let destination = root.join("packages").join(fixture);
    let mut args = vec!["run".as_ref(),
                        "--locked".as_ref(),
                        "--manifest-path".as_ref(),
                        manifest.as_os_str(),
                        "--bin".as_ref(),
                        "package".as_ref(),
                        "--target-dir".as_ref(),
                        target.as_os_str(),
                        "--".as_ref(),
                        elf.as_os_str()];
    let base = root.join("base-System.bin");
    let system = destination.join("System.bin");
    fs::create_dir_all(&destination).unwrap();
    if fixture == "definition-panic"
    {
        args.extend([base.as_os_str(), system.as_os_str()]);
    }
    else
    {
        args.push(destination.as_os_str());
    }
    run(repo, root, &target, "host", &args, &format!("{fixture}-package"), report);
    if fixture == "definition-panic" { system } else { destination.join("Finder.bin") }
}
fn artifact(output: &Output, name: &str) -> Value
{
    String::from_utf8_lossy(&output.stdout).lines()
                                           .filter_map(|line| {
                                               serde_json::from_str::<Value>(line).ok()
                                           })
                                           .find(|v| {
                                               v["reason"] == "compiler-artifact"
                                               && v["target"]["name"] == name
                                               && v["filenames"].as_array().is_some_and(|files|files.iter().any(|file|file.as_str().is_some_and(|path|path.contains("riscv64imac-unknown-none-elf"))))
                                           })
                                           .unwrap_or_else(|| {
                                               panic!("no compiler artifact for {name}")
                                           })
}
fn verify_package(repo: &Path, root: &Path, fixture: &str, path: &Path)
{
    let manifest_path =
        repo.join("crates/mac-toolbox/tests/fixtures").join(fixture).join("resources.json");
    let manifest = mac_build::Manifest::read(&manifest_path).unwrap();
    let file = macbinary::decode(&fs::read(path).unwrap()).unwrap();
    assert_eq!(file.creator, manifest.creator.as_bytes());
    let actual = resource::decode(&file.resources).unwrap();
    let runtime = root.join("packages").join(fixture).join("Finder.runtime.elf");
    let expected = manifest.compile_resources(&manifest_path,
                                              &fs::read(runtime).unwrap(),
                                              Some(&root.join("fixtures")
                                                        .join(fixture)
                                                        .join("riscv64imac-unknown-none-elf/\
                                                               release")))
                           .unwrap();
    for expected in expected
    {
        let actual =
            actual.iter().find(|r| r.kind == expected.kind && r.id == expected.id).unwrap();
        assert_eq!(actual, &expected, "packaged resource came from another fixture");
    }
    assert!(actual.iter().any(|r| r.kind == *b"RLOC" && !r.data.is_empty()));
}
fn watch_files(directory: &Path, contents: &mut Vec<String>)
{
    for entry in fs::read_dir(directory).unwrap()
    {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir()
        {
            watch_files(&entry.path(), contents);
        }
        else if entry.file_name() == "stdout"
        {
            contents.push(fs::read_to_string(entry.path()).unwrap());
        }
    }
}
#[test]
#[ignore = "builds three real standalone RV64 fixtures in an explicitly selected private build root"]
fn standalone_fixtures_share_dependencies_without_sharing_final_binaries_or_native_resources()
{
    let root=PathBuf::from(std::env::var_os("SYSTEM7_FIXTURE_CACHE_CHECK_ROOT").expect("set SYSTEM7_FIXTURE_CACHE_CHECK_ROOT to a private build directory"));
    assert!(root.is_absolute(), "private fixture cache check root must be absolute");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).unwrap();
    assert!(!root.starts_with(repo), "generated build outputs must be outside source");
    fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let mut report = Vec::new();
    let first = build(repo, &root, "conformance", "Finder", "guest", &mut report);
    let second = build(repo, &root, "retained-update", "Finder", "guest", &mut report);
    let a = artifact(&first, "mac_abi");
    let b = artifact(&second, "mac_abi");
    assert_eq!(b["fresh"], true, "second fixture rebuilt the common SDK dependency");
    assert_eq!(a["filenames"], b["filenames"],
               "common dependencies did not use the same intermediate artifact");
    assert!(b["filenames"].as_array().unwrap().iter().all(|p|Path::new(p.as_str().unwrap()).starts_with(root.join("fixture-cache"))));
    let elf = |name: &str| {
        root.join("fixtures").join(name).join("riscv64imac-unknown-none-elf/release/Finder")
    };
    let original_a = fs::read(elf("conformance")).unwrap();
    let original_b = fs::read(elf("retained-update")).unwrap();
    assert_ne!(original_a, original_b, "different Finder programs share the same binary");
    let repeated = build(repo, &root, "conformance", "Finder", "guest", &mut report);
    assert_eq!(artifact(&repeated, "mac_abi")["fresh"], true);
    assert_eq!(fs::read(elf("conformance")).unwrap(), original_a);
    assert_eq!(fs::read(elf("retained-update")).unwrap(), original_b);
    // A valid Cargo cache selection needs no final-artifact override when this
    // manifest contains no separately built native-code resource payload.
    let cache_only = build(repo, &root, "conformance", "Finder", "guest-cache-only", &mut report);
    assert_eq!(artifact(&cache_only, "mac_abi")["fresh"], true);
    assert_eq!(artifact(&cache_only, "Finder")["fresh"],
               true,
               "an unused artifact override rebuilt a program with no native-code payloads");
    assert_eq!(fs::read(elf("conformance")).unwrap(), original_a);
    for fixture in ["conformance", "retained-update"]
    {
        let packaged = package(repo, &root, fixture, "Finder", &mut report);
        verify_package(repo, &root, fixture, &packaged);
    }
    // No OS disk is needed to test replacement. The real definition-panic
    // packager replaces one CDEF in this small native resource-fork container.
    let base = MacFile { name:      b"System".to_vec(),
                         file_type: *b"zsys",
                         creator:   *b"MACS",
                         flags:     0,
                         created:   0,
                         modified:  0,
                         data:      Vec::new(),
                         resources:
                             resource::encode_native(&[Resource { kind:       *b"CDEF",
                                                                  id:         -32759,
                                                                  name:       None,
                                                                  attributes: 0,
                                                                  data:       vec![0] }]).unwrap() };
    fs::write(root.join("base-System.bin"), macbinary::encode(&base).unwrap()).unwrap();
    build(repo, &root, "definition-panic", "CDEF_PANIC", "guest", &mut report);
    let packaged = package(repo, &root, "definition-panic", "CDEF_PANIC", &mut report);
    let final_elf =
        root.join("fixtures/definition-panic/riscv64imac-unknown-none-elf/release/CDEF_PANIC");
    let manifest_path =
        repo.join("crates/mac-toolbox/tests/fixtures/definition-panic/resources.json");
    let expected =
        mac_build::Manifest::read(&manifest_path).unwrap()
                                                 .compile_resources(&manifest_path,
                                                                    &fs::read(&final_elf).unwrap(),
                                                                    final_elf.parent())
                                                 .unwrap();
    let expected = expected.iter().find(|r| r.kind == *b"CDEF" && r.id == -32759).unwrap();
    let file = macbinary::decode(&fs::read(packaged).unwrap()).unwrap();
    assert_eq!(resource::decode(&file.resources).unwrap(), [expected.clone()]);
    let mut watches = Vec::new();
    watch_files(&root.join("fixture-cache/riscv64imac-unknown-none-elf/release/build/\
                            definition-panic-probe"),
                &mut watches);
    assert!(watches.iter().any(|text| {
                              text.lines().any(|line| {
                                              line
                                              == format!("cargo:rerun-if-changed={}",
                                                         final_elf.display())
                                          })
                          }),
            "native-code source watch did not name the actual final ELF");
    assert!(watches.iter().any(|text| {
                              text.contains("cargo:rerun-if-env-changed=SYSTEM7_CARGO_ARTIFACT_DIR")
                          }));
    report.push(json!({"common_dependency_fresh":true,"common_artifact":b["filenames"],"first_elf_sha256":mac_build::package::sha256(&original_a),"second_elf_sha256":mac_build::package::sha256(&original_b),"definition_final_elf":final_elf,"native_definition_package":"verified"}));
    fs::write(root.join("acceptance.json"), serde_json::to_vec_pretty(&report).unwrap()).unwrap();
}
