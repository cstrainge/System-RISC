fn main()
{
    println!("cargo:rerun-if-env-changed={}", mac_build::platform::PLATFORM_ENV);
    let platform = mac_build::platform::current();
    let definition =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../platforms")
                                                        .join(format!("{}.json", platform.name));
    println!("cargo:rerun-if-changed={}", definition.display());
    let checked =
        platform.memory().expect("System platform layout must validate before compilation");
    std::fs::write(std::path::Path::new(&std::env::var_os("OUT_DIR").expect("Cargo supplies OUT_DIR"))
        .join("platform-display.rs"), checked.display_constants())
        .expect("Cargo output directory must accept checked display constants");
    let scheduling =
        platform.scheduling()
                .expect("System platform scheduling policy must validate before compilation");
    std::fs::write(std::path::Path::new(&std::env::var_os("OUT_DIR").expect("Cargo supplies OUT_DIR"))
        .join("platform-scheduling.rs"), scheduling.rust_constants())
        .expect("Cargo output directory must accept checked scheduling constants");
    if std::env::var("TARGET") != std::env::var("HOST")
    {
        mac_build::build_guest().expect("System build configuration");
    }
}
