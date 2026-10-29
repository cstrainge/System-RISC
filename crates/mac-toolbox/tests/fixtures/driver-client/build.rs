#[path = "scheduler_manifest.rs"]
mod scheduler_manifest;
#[path = "pressure_manifest.rs"]
mod pressure_manifest;
#[path = "smp_manifest.rs"]
mod smp_manifest;
fn main()
{
    if std::env::var("TARGET").unwrap() == "riscv64imac-unknown-none-elf"
    {
        mac_build::build_guest().expect("module build");
        if std::env::var_os("CARGO_FEATURE_SMP").is_some()
        {
            assert!(std::env::var_os("CARGO_FEATURE_WORKERS").is_none()
                    && std::env::var_os("CARGO_FEATURE_SCHEDULER").is_none(),
                    "SMP is a distinct native diagnostic package");
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
            let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
            let path = smp_manifest::prepare(root, &out.join("smp"));
            let manifest = mac_build::Manifest::read(&path).unwrap();
            std::fs::write(out.join("module_contract.bin"),
                           mac_build::module::declaration(&path, &manifest).unwrap()).unwrap();
            println!("cargo:rerun-if-changed=module.smp.json");
            println!("cargo:rerun-if-changed=resources.smp.json");
        }
        if std::env::var_os("CARGO_FEATURE_SCHEDULER").is_some()
        {
            assert!(std::env::var_os("CARGO_FEATURE_PRESSURE").is_none()
                    && std::env::var_os("CARGO_FEATURE_WORKERS").is_none(),
                    "scheduler is a distinct native diagnostic package");
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
            let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
            let path = scheduler_manifest::prepare(root, &out.join("scheduler"));
            let manifest = mac_build::Manifest::read(&path).unwrap();
            let bytes = mac_build::module::declaration(&path, &manifest).unwrap();
            std::fs::write(out.join("module_contract.bin"), bytes).unwrap();
            println!("cargo:rerun-if-changed=module.scheduler.json");
            println!("cargo:rerun-if-changed=resources.scheduler.json");
        }

        println!("cargo:rerun-if-env-changed=SYSTEM7_PRESSURE_IDENTITY");
        println!("cargo:rerun-if-env-changed=SYSTEM7_PRESSURE_NAME");
        println!("cargo:rerun-if-env-changed=SYSTEM7_PRESSURE_ORDINAL");
        println!("cargo:rustc-env=SYSTEM7_PRESSURE_ORDINAL={}",
                 std::env::var("SYSTEM7_PRESSURE_ORDINAL").unwrap_or_else(|_| "0".into()));
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

        if std::env::var_os("CARGO_FEATURE_WORKERS").is_some()
           && std::env::var_os("CARGO_FEATURE_PRESSURE").is_none()
        {
            let path = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("resources.workers.json");
            println!("cargo:rerun-if-changed={}", path.display());
            let manifest = mac_build::Manifest::read(&path).unwrap();
            let bytes = mac_build::module::declaration(&path, &manifest).unwrap();
            std::fs::write(std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("module_contract.bin"), bytes).unwrap();
        }
    }
}
