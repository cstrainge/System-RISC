fn main()
{
    if cfg!(feature = "pressure")
    {
        package_pressure();
        return;
    }
    let args: Vec<_> = std::env::args_os().collect();
    mac_build::package::build(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources.json"),
                              std::path::Path::new(&args[1]),
                              std::path::Path::new(&args[2])).expect("package private SDK fixture");
    use mac_formats::{macbinary::{self, MacFile},
                      resource::{self, Resource}};
    let output = std::path::Path::new(&args[2]);
    if let Some(system) = args.get(3)
    {
        use mac_formats::authority;
        // Explicit image-bound grant only on this private observer disk. The
        // independently packaged workers retain ordinary own-domain access.
        let application = macbinary::decode(&std::fs::read(output.join("Finder.bin")).expect("read observer package")).expect("decode observer package");
        let digest = authority::image_digest(&resource::decode(&application.resources).expect("decode observer resources")).expect("digest observer image");
        let mut file = macbinary::decode(&std::fs::read(system).expect("read current System package")).expect("decode current System package");
        let mut resources =
            resource::decode(&file.resources).expect("decode current System resources");
        resources.iter_mut()
                 .find(|resource| resource.kind == authority::RESOURCE_KIND && resource.id == 0)
                 .expect("System authority policy")
                 .data =
            authority::encode(&[(digest, authority::INSPECT_DOMAINS)]).expect("encode private \
                                                                               observer grant");
        file.resources =
            resource::encode_native(&resources).expect("encode observer policy System resources");
        std::fs::write(output.join("System.bin"), macbinary::encode(&file).expect("encode private observer System package")).expect("write private observer System package");
        resources.iter_mut()
                 .find(|resource| resource.kind == *b"DLOG" && resource.id == -6042)
                 .expect("System Open layout")
                 .data
                 .clear();
        file.resources =
            resource::encode_native(&resources).expect("encode malformed-layout System resources");
        std::fs::write(output.join("Malformed System.bin"), macbinary::encode(&file).expect("encode private System package")).expect("write malformed-layout System package");
    }
    let budget=resource::encode(&[Resource {kind:*b"DATA",id:1,name:None,attributes:0,
        data:vec![0;4*1024*1024+1]}]).expect("encode valid resource fork above application payload budget");
    let packaged = macbinary::decode(&std::fs::read(output.join("Finder.bin")).expect("read observer package"))
        .expect("decode observer package");
    let mut missing = resource::decode(&packaged.resources).expect("decode observer resources");
    missing.retain(|resource| resource.kind != *b"RLOC");
    let mut malformed = missing.clone();
    malformed.push(Resource { kind:       *b"RLOC",
                              id:         0,
                              name:       None,
                              attributes: 0,
                              data:       vec![0] });
    let missing = resource::encode_native(&missing).expect("encode missing relocation fixture");
    let malformed =
        resource::encode_native(&malformed).expect("encode malformed relocation fixture");
    for (name, resources) in [("Budget App", budget),
                              ("Corrupt App", vec![0; 16]),
                              ("Missing Code", missing),
                              ("Malformed Code", malformed)]
    {
        let bytes = macbinary::encode(&MacFile { name: name.as_bytes().to_vec(),
                                                 file_type: *b"APPL",
                                                 creator: *b"PRTS",
                                                 flags: 0,
                                                 created: 0,
                                                 modified: 0,
                                                 data: Vec::new(),
                                                 resources }).expect("encode format-error \
                                                                      application fixture");
        std::fs::write(output.join(format!("{name}.bin")), bytes).expect("write format-error \
                                                                          fixture");
    }
}

fn package_pressure()
{
    use mac_formats::{authority, macbinary, resource};
    let args: Vec<_> = std::env::args_os().collect();
    let output = std::path::Path::new(&args[2]);
    mac_build::package::build(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources.pressure.json"), std::path::Path::new(&args[1]), output).expect("package real pressure observer");
    let app =
        macbinary::decode(&std::fs::read(output.join("Pressure Probe.bin")).unwrap()).unwrap();
    let digest = authority::image_digest(&resource::decode(&app.resources).unwrap()).unwrap();
    let mut system =
        macbinary::decode(&std::fs::read(&args[3]).expect("current private System package \
                                                           required")).unwrap();
    let mut resources = resource::decode(&system.resources).unwrap();
    let policy =
        resources.iter_mut().find(|r| r.kind == authority::RESOURCE_KIND && r.id == 0).unwrap();
    // Validate and preserve all existing exact-image grants, including Finder.
    authority::granted(&policy.data, digest).unwrap();
    let mut grants: Vec<([u8; 32], u64)> =
        policy.data[12..].chunks_exact(40)
                         .map(|row| {
                             (row[..32].try_into().unwrap(),
                              u64::from_be_bytes(row[32..].try_into().unwrap()))
                         })
                         .filter(|(old, _)| *old != digest)
                         .collect();
    grants.push((digest, authority::INSPECT_DOMAINS));
    policy.data = authority::encode(&grants).unwrap();
    system.resources = resource::encode_native(&resources).unwrap();
    std::fs::write(output.join("System.bin"), macbinary::encode(&system).unwrap()).unwrap();
    std::fs::write(output.join("pressure-actor-grant.txt"),
                   format!("exact_image_digest={digest:02x?} capability=INSPECT_DOMAINS \
                            existing_grants_preserved=true\n")).unwrap();
}
