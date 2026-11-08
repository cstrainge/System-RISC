fn main()
{
    println!("cargo:rustc-check-cfg=cfg(systemrisc)");
    // The native half of every `#[cfg(systemrisc)]` split in this crate. Every
    // other guest crate emits this and this one did not, so the host branch of
    // all 795 sites was compiled into the guest build. The visible consequence
    // was that `firmware_call` took its `#[cfg(not(systemrisc))]` arm and called
    // firmware pointers without the translation-root switch `with_firmware`
    // exists to provide, which faulted on the first storage callback.
    if std::env::var("TARGET") != std::env::var("HOST")
    {
        println!("cargo:rustc-cfg=systemrisc");
    }
    println!("cargo:rerun-if-env-changed={}", mac_build::platform::PLATFORM_ENV);
    let platform = mac_build::platform::current();
    for definition in mac_build::platform::record_files(&platform.name).expect("System platform \
                                                                                source records \
                                                                                must resolve")
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
    let module_target = mac_build::platform::module_target_constants().expect("System platform \
                                                                               module target \
                                                                               must resolve");
    mac_build::write_if_changed(std::path::Path::new(&std::env::var_os("OUT_DIR").expect("Cargo supplies OUT_DIR"))
        .join("platform-module-target.rs"), module_target)
        .expect("Cargo output directory must accept the module contract target");
    if std::env::var("TARGET") != std::env::var("HOST")
    {
        mac_build::build_guest().expect("System build configuration");
    }
}
