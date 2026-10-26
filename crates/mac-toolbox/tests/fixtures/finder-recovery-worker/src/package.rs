fn main()
{
    let args: Vec<_> = std::env::args_os().collect();
    let manifest = if args.get(3).is_some_and(|arg| arg == "--image-pressure")
    {
        "resources-pressure.json"
    }
    else
    {
        "resources.json"
    };
    mac_build::package::build(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(manifest),
                              std::path::Path::new(&args[1]),
                              std::path::Path::new(&args[2])).expect("package private recovery \
                                                                      fixture");
}
