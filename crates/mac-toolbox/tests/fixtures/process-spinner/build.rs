fn main()
{
    if std::env::var("TARGET").unwrap() == "riscv64imac-unknown-none-elf"
    {
        mac_build::build_guest().expect("public process fixture resources");
        println!("cargo:rerun-if-changed=resources.smp.json");
    }
}
