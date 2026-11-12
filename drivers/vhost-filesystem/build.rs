fn main()
{
    // Every guest target needs the module contract and the relocatable image, not
    // only RISC-V: the contract is what the loader binds, and it is the same
    // record on any architecture.
    if std::env::var("TARGET").unwrap() != std::env::var("HOST").unwrap()
    {
        mac_build::build_guest().expect("host filesystem module build");
    }
}
