fn main()
{
    let args: Vec<_> = std::env::args_os().collect();
    mac_build::package::build(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources.json"),
                              std::path::Path::new(&args[1]),
                              std::path::Path::new(&args[2])).expect("package private SDK fixture");
    if let Some(path) = args.get(3)
    {
        // Explicit private-test policy grant, bound to the freshly packaged
        // resource content. No application name/creator bypass is introduced.
        use mac_formats::{authority, macbinary, resource};
        let output = std::path::Path::new(&args[2]);
        let application =
            macbinary::decode(&std::fs::read(output.join("Finder.bin")).unwrap()).unwrap();
        let digest =
            authority::image_digest(&resource::decode(&application.resources).unwrap()).unwrap();
        let mut system = macbinary::decode(&std::fs::read(path).unwrap()).unwrap();
        let mut resources = resource::decode(&system.resources).unwrap();
        let policy =
            resources.iter_mut().find(|r| r.kind == authority::RESOURCE_KIND && r.id == 0).unwrap();
        policy.data = authority::encode(&[(digest, authority::INSPECT_DOMAINS)]).unwrap();
        system.resources = resource::encode(&resources).unwrap();
        std::fs::write(output.join("System.bin"), macbinary::encode(&system).unwrap()).unwrap();
    }
}
