fn main()
{
    // This package is the AArch64 machine's power mechanism and nothing else: the
    // contract and the relocatable image are built for that machine's target, so
    // a build aimed anywhere else is a build that asked the wrong package.
    if std::env::var("TARGET").unwrap() == "aarch64-unknown-none"
    {
        mac_build::build_guest().expect("PSCI power controller contract and relocatable image \
                                         must validate");
    }
}
