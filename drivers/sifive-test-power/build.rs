fn main()
{
    // This package is the RISC-V machine's power mechanism and nothing else: the
    // contract and the relocatable image are built for that machine's target, so
    // a build aimed anywhere else is a build that asked the wrong package.
    if std::env::var("TARGET").unwrap() == "riscv64imac-unknown-none-elf"
    {
        mac_build::build_guest().expect("SiFive test power controller contract and relocatable \
                                         image must validate");
    }
}
