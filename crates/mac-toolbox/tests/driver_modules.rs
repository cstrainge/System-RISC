//! Generic module SDK conformance through actual native resource loading. These
//! assertions concern the kernel-module SDK; volume APIs get separate userspace
//! File/Resource Manager acceptance when the filesystem classes are installed.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs,
          path::{Path, PathBuf},
          process::Command};

use mac_formats::{macbinary, native, resource};
use serde_json::json;
fn python(script: &Path, args: &[&std::ffi::OsStr])
{
    let output = Command::new("python3").arg(script).args(args).output().unwrap();
    assert!(output.status.success(),
            "transport:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr));
}
fn digest_into(data: &mut [u8], code: &[u8])
{
    let hash = mac_build::package::sha256(code);
    for index in 0..32
    {
        data[88 + index] = u8::from_str_radix(&hash[index * 2..index * 2 + 2], 16).unwrap();
    }
}
#[test]
#[ignore = "requires fresh private cargo xtask image and QEMU"]
fn native_modules_relocate_resolve_dependencies_and_reject_bad_packages_before_entry()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output =
        root.join("results/system7").join(format!("driver-modules-{}", std::process::id()));
    let script = root.join("System7/scripts/capture_driver_modules.py");
    python(&script, &["--output".as_ref(), output.as_os_str(), "--prepare".as_ref()]);
    let provider =
        macbinary::decode(&fs::read(output.join("packages/ModuleEcho.bin")).unwrap()).unwrap();
    let client = output.join("packages/ModuleClient.bin");
    let resources = resource::decode(&provider.resources).unwrap();
    let package = mac_formats::module::Package::inspect(&resources, *b"DRVR").unwrap();
    assert_eq!(package.code.descriptor.role, native::Role::CodeResource);
    assert_eq!(package.code.descriptor.load_base, 0x82000000);
    assert!(!resources.iter().any(|r| [*b"RV64", *b"NABI", *b"RLOC"].contains(&r.kind)));
    let mut cases = Vec::new();
    for name in ["success", "wrong-directory", "missing-contract", "duplicate-contract",
                 "wrong-abi", "wrong-target", "wrong-layout", "bad-digest", "bad-relocation"]
    {
        let mut entries = resources.clone();
        let contract = entries.iter().position(|r| r.kind == *b"DABI").unwrap();
        let code = entries.iter().position(|r| r.kind == *b"DRVR").unwrap();
        match name
        {
            "missing-contract" =>
            {
                entries.remove(contract);
            }
            "duplicate-contract" =>
            {
                let mut duplicate = entries[contract].clone();
                duplicate.id = 1;
                entries.push(duplicate);
            }
            "wrong-abi" =>
            {
                entries[contract].data[16..20].copy_from_slice(&(native::ABI_VERSION+1).to_le_bytes());
            }
            "wrong-target" =>
            {
                entries[contract].data[24..26].copy_from_slice(&mac_module::Target::X86_64.to_le_bytes());
            }
            "wrong-layout" =>
            {
                entries[contract].data[120] ^= 1;
            }
            "bad-digest" =>
            {
                entries[code].data[68] ^= 1;
            }
            "bad-relocation" =>
            {
                let descriptor = native::Descriptor::decode(&entries[code].data[..40]).unwrap();
                let body = &entries[code].data[40..];
                let end = native::image::inspect(body.len(),
                                                 &descriptor,
                                                 |at, out| {
                                                     out.copy_from_slice(&body[at..at + out.len()]);
                                                     Ok(())
                                                 },
                                                 |_| {}).unwrap();
                entries[code].data[40 + end] ^= 1;
                let bytes = entries[code].data.clone();
                digest_into(&mut entries[contract].data, &bytes);
            }
            _ =>
            {}
        }
        if !matches!(name, "success" | "wrong-directory" | "wrong-target" | "wrong-abi")
        {
            assert!(mac_formats::module::Package::inspect(&entries, *b"DRVR").is_err(), "{name}");
        }
        let mut file = provider.clone();
        file.resources = resource::encode(&entries).unwrap();
        let path = output.join(format!("{name}.bin"));
        fs::write(&path, macbinary::encode(&file).unwrap()).unwrap();
        cases.push(json!({"name":name,"files":[
            {"source":path,"directory":if name=="wrong-directory"{"/System Folder/Extensions"}else{"/System Folder/Drivers"}},
            {"source":client,"directory":"/System Folder/Drivers"}]}));
    }
    let case_path = output.join("cases.json");
    fs::write(&case_path, serde_json::to_vec_pretty(&cases).unwrap()).unwrap();
    python(&script,
           &["--output".as_ref(), output.as_os_str(), "--cases".as_ref(), case_path.as_os_str()]);
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["original_before"], report["original_after"]);
    for case in &cases
    {
        let name = case["name"].as_str().unwrap();
        let serial = fs::read_to_string(output.join(name).join("vm/serial.log")).unwrap();
        assert!(serial.contains("DESKTOP_READY"), "{name}: {serial}");
        if name == "success"
        {
            assert!(serial.contains("MODULE_PUBLISHED name=ModuleEcho"), "{serial}");
            assert!(serial.contains("MODULE_PUBLISHED name=ModuleClient"), "{serial}");
            let echo = serial.find("MODULE_PUBLISHED name=ModuleEcho").unwrap();
            let client = serial.find("MODULE_ENTER name=ModuleClient").unwrap();
            assert!(echo < client, "dependency entered before provider was ready: {serial}");
            assert!(!serial.contains("MODULE_REJECTED"), "{serial}");
        }
        else
        {
            assert!(!serial.contains("MODULE_ENTER"),
                    "rejected package entered native code: {name}: {serial}");
            assert!(serial.contains("MODULE_REJECTED"), "no rejection evidence: {name}: {serial}");
        }
        assert!(!serial.contains("SYSTEM_TRAP:"), "{serial}");
    }
    let acceptance = json!({"status":"passed","cases":cases.len(),"kernel_module_sdk":"actual DRVR entries and declared-dependency broker calls",
        "application_filesystem_mounts":"not covered by this gate","capture":report});
    fs::write(output.join("rust-acceptance.json"),serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
    fs::write(root.join("results/system7/driver-module-checks.json"),
              serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
}
