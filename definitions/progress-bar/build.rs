fn main()
{
    if std::env::var("TARGET") != std::env::var("HOST")
    {
        mac_build::build_guest().expect("Progress Bar CDEF build configuration");
    }
}
