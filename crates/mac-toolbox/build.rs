fn main()
{
    println!("cargo:rustc-check-cfg=cfg(systemrisc)");
    if std::env::var("TARGET") != std::env::var("HOST")
    {
        println!("cargo:rustc-cfg=systemrisc");
    }
}
