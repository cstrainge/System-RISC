fn main()
{
    println!("cargo:rerun-if-env-changed={}", mac_build::platform::PLATFORM_ENV);
    let platform = mac_build::platform::current();
    for definition in mac_build::platform::record_files(&platform.name)
        .expect("System platform source records must resolve")
    {
        println!("cargo:rerun-if-changed={}", definition.display());
    }
    let checked =
        platform.memory().expect("System platform layout must validate before compilation");
    mac_build::write_if_changed(std::path::Path::new(&std::env::var_os("OUT_DIR").expect("Cargo supplies OUT_DIR"))
        .join("platform-display.rs"), checked.display_constants())
        .expect("Cargo output directory must accept checked display constants");
    let scheduling =
        platform.scheduling()
                .expect("System platform scheduling policy must validate before compilation");
    mac_build::write_if_changed(std::path::Path::new(&std::env::var_os("OUT_DIR").expect("Cargo supplies OUT_DIR"))
        .join("platform-scheduling.rs"), scheduling.rust_constants())
        .expect("Cargo output directory must accept checked scheduling constants");
    if std::env::var("TARGET") != std::env::var("HOST")
    {
        mac_build::build_guest().expect("System build configuration");
    }
}
