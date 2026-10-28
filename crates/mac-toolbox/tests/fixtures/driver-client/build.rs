fn main()
{
    if std::env::var("TARGET").unwrap() == "riscv64imac-unknown-none-elf"
    {
        mac_build::build_guest().expect("module build");
        if std::env::var_os("CARGO_FEATURE_WORKERS").is_some()
        {
            let path = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("resources.workers.json");
            println!("cargo:rerun-if-changed={}", path.display());
            let manifest = mac_build::Manifest::read(&path).unwrap();
            let bytes = mac_build::module::declaration(&path, &manifest).unwrap();
            std::fs::write(std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("module_contract.bin"), bytes).unwrap();
        }
    }
}
