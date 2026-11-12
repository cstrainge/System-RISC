fn main()
{
    // The contract and the relocatable image are the same record on any guest
    // architecture: the controller is a memory-mapped core, and nothing about
    // driving it belongs to one processor.
    if std::env::var("TARGET").unwrap() != std::env::var("HOST").unwrap()
    {
        mac_build::build_guest().expect("DWC2 USB contract and relocatable image must validate");
    }
}
