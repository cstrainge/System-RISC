fn main()
{
    if std::env::var("TARGET") != std::env::var("HOST")
    {
        mac_build::build_guest().expect("ListBox CDEF build configuration invariant");
    }
}
