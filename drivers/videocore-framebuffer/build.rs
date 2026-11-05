fn main()
{
    if std::env::var("TARGET").unwrap() == "aarch64-unknown-none"
    {
        mac_build::build_guest().expect("VideoCore Framebuffer contract and relocatable image \
                                         must validate");
    }
}
