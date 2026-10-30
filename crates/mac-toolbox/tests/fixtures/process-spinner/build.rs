fn main()
{
    assert!(!(cfg!(feature = "heap") && cfg!(feature = "smp")),
            "heap and smp select distinct native ProcessSpinner workloads; choose only one");
    if std::env::var("TARGET").unwrap() == "riscv64imac-unknown-none-elf"
    {
        mac_build::build_guest().expect("public process fixture resources");
        println!("cargo:rerun-if-changed=resources.smp.json");
        println!("cargo:rerun-if-changed=resources.heap.json");
    }
}
