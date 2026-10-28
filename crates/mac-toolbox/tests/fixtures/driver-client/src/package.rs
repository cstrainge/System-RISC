fn main()
{
    let args: Vec<_> = std::env::args_os().collect();
    mac_build::package::build(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(if cfg!(feature = "workers") { "resources.workers.json" } else { "resources.json" }).as_path(),std::path::Path::new(&args[1]),std::path::Path::new(&args[2])).unwrap();
}
