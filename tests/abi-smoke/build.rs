use std::{env, fs, path::Path};
fn main()
{
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_FEATURE");
    if env::var("TARGET") != env::var("HOST")
    {
        mac_build::platform::validate_guest(&env::var("TARGET").unwrap(),&env::var("CARGO_CFG_TARGET_FEATURE").unwrap()).expect("native platform ISA floor; use cargo xtask");
    }
    let output = Path::new(&env::var("OUT_DIR").unwrap()).to_owned();
    fs::write(output.join("platform-virtual.rs"),
              mac_build::platform::current().virtual_memory()
                                            .expect("ABI virtual policy")
                                            .rust_constants_for(&["VM_SYSTEM_IMAGE_BASE",
                                                                  "VM_SYSTEM_IMAGE_LIMIT",
                                                                  "VM_PHYSMAP_START",
                                                                  "VM_PHYSMAP_END",
                                                                  "VM_PHYSMAP_PHYSICAL_BASE"])
                                            .expect("checked bootstrap virtual symbols")).unwrap();
    let (platform, symbols) = mac_build::platform::guest_artifacts().expect("ABI device inventory");
    fs::write(output.join("platform.rs"), platform).unwrap();
    println!("cargo:rerun-if-env-changed=SYSTEM7_SYSTEM_REGIONS");
    let regions =
        std::path::PathBuf::from(env::var_os("SYSTEM7_SYSTEM_REGIONS").expect("xtask supplies \
                                                                               System segment \
                                                                               records"));
    println!("cargo:rerun-if-changed={}", regions.display());
    fs::copy(regions, output.join("system-regions.rs")).unwrap();
    println!("cargo:rerun-if-env-changed=SYSTEM7_PAGE_PROBE");
    let probe = env::var("SYSTEM7_PAGE_PROBE").unwrap_or_default();
    let (address, write) = match probe.as_str()
    {
        "" => ("0", false),
        "lower_guard" => ("platform_virtual::VM_SYSTEM_STACKS_START", false),
        "upper_guard" =>
        {
            ("platform_virtual::VM_SYSTEM_STACKS_START + platform_memory::PAGE_BYTES + \
              PROBE_SYSTEM_STACK_BYTES",
             false)
        }
        "code_write" => ("SYSTEM_ENTRY as u64", true),
        _ =>
        {
            panic!("unknown native page probe {probe}; select lower_guard, upper_guard or \
                    code_write")
        }
    };
    if !probe.is_empty() && probe != "code_write"
    {
        let mut virtual_constants = fs::read_to_string(output.join("platform-virtual.rs")).unwrap();
        virtual_constants.push_str(&mac_build::platform::current().virtual_memory().unwrap()
            .rust_constants_for(&["VM_SYSTEM_STACKS_START"]).unwrap());
        fs::write(output.join("platform-virtual.rs"), virtual_constants).unwrap();
    }
    let value = if probe.is_empty()
    {
        "None".to_owned()
    }
    else
    {
        format!("Some(({probe:?}, {address}, {write}))")
    };
    let mut constants = format!("const PAGE_PROBE: Option<(&str,u64,bool)> = {value};\n");
    if probe == "upper_guard"
    {
        constants.push_str(&format!("const PROBE_SYSTEM_STACK_BYTES: u64 = {};\n",
                                    mac_build::platform::current().memory()
                                                                  .unwrap()
                                                                  .system_stack_default()));
    }
    for name in ["SYSTEM", "FINDER"]
    {
        let key = format!("SYSTEM7_{name}_ENTRY");
        println!("cargo:rerun-if-env-changed={key}");
        let value: u64 = env::var(&key).expect("use cargo xtask check to supply native entry \
                                                addresses")
                                       .parse()
                                       .unwrap();
        constants.push_str(&format!("const {name}_ENTRY: usize = {value};\n"));
    }
    fs::write(Path::new(&env::var("OUT_DIR").unwrap()).join("entries.rs"), constants).unwrap();
    let script = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("link.ld");
    println!("cargo:rerun-if-changed={}", script.display());
    let generated = output.join("probe.ld");
    fs::write(&generated, symbols + &fs::read_to_string(&script).unwrap()).unwrap();
    println!("cargo:rustc-link-arg=-T{}", generated.display());
    println!("cargo:rustc-link-arg=--no-relax");
}
