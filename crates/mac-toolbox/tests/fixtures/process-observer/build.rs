fn main()
{
    if std::env::var("TARGET").unwrap() == "riscv64imac-unknown-none-elf"
    {
        mac_build::build_guest().expect("public process fixture resources");
        // The pressure actor retains the same native entry/target contract.
        println!("cargo:rerun-if-changed=resources.pressure.json");
    }
}
