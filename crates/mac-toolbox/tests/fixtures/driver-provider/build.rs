#[path = "scheduler_manifest.rs"]
mod scheduler_manifest;
#[path = "pressure_manifest.rs"]
mod pressure_manifest;
fn main()
{
    if std::env::var("TARGET").unwrap() == "riscv64imac-unknown-none-elf"
    {
        mac_build::build_guest().expect("module build");
        if std::env::var_os("CARGO_FEATURE_SCHEDULER").is_some()
        {
            assert!(std::env::var_os("CARGO_FEATURE_PRESSURE").is_none()
                    && std::env::var_os("CARGO_FEATURE_WORKERS").is_none(),
                    "scheduler is a distinct native diagnostic package");
            let variants = ["CARGO_FEATURE_SCHEDULER_DENY", "CARGO_FEATURE_SCHEDULER_MALFORMED",
                            "CARGO_FEATURE_SCHEDULER_NO_DEFER", "CARGO_FEATURE_SCHEDULER_SMP"];
            assert!(variants.into_iter().filter(|name| std::env::var_os(name).is_some()).count()
                    <= 1,
                    "choose one native scheduler guard diagnostic at a time");
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
