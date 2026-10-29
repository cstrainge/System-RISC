fn main()
{
    if std::env::var("TARGET").as_deref() == Ok("riscv64imac-unknown-none-elf")
    {
        mac_build::build_guest().expect("Multi-Scheduler native module build invariant");
    }
}
