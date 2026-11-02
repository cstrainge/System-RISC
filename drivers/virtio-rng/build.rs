fn main()
{
    if std::env::var("TARGET").unwrap() == "riscv64imac-unknown-none-elf"
    {
        mac_build::build_guest().expect("VirtIO RNG contract and relocatable image must validate");
    }
}
