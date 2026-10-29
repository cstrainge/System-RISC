#[path = "pressure_manifest.rs"]
mod pressure_manifest;
fn main()
{
    if std::env::var("TARGET").unwrap() == "riscv64imac-unknown-none-elf"
    {
        mac_build::build_guest().expect("module build");
        println!("cargo:rerun-if-env-changed=SYSTEM7_PRESSURE_IDENTITY");
        println!("cargo:rerun-if-env-changed=SYSTEM7_PRESSURE_NAME");
        println!("cargo:rerun-if-changed=module.pressure.json");
        println!("cargo:rerun-if-changed=resources.pressure.json");
        if std::env::var_os("CARGO_FEATURE_PRESSURE").is_some()
        {
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
            let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
            let path = pressure_manifest::prepare(root, &out.join("pressure"));
            let manifest = mac_build::Manifest::read(&path).unwrap();
            let bytes = mac_build::module::declaration(&path, &manifest).unwrap();
            std::fs::write(out.join("module_contract.bin"), bytes).unwrap();
        }
    }
}
