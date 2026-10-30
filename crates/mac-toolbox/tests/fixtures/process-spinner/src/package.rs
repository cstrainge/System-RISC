fn main()
{
    let args: Vec<_> = std::env::args_os().collect();
    let manifest = if cfg!(feature = "exit-late")
    {
        "resources.exit-late.json"
    }
    else if cfg!(feature = "exit")
    {
        "resources.exit.json"
    }
    else if cfg!(feature = "tls")
    {
        "resources.tls.json"
    }
    else if cfg!(feature = "heap")
    {
        "resources.heap.json"
    }
    else if cfg!(feature = "smp")
    {
        "resources.smp.json"
    }
    else
    {
        "resources.json"
    };
    mac_build::package::build(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(manifest),
                              std::path::Path::new(&args[1]),
                              std::path::Path::new(&args[2])).expect("package private SDK fixture");
}
