fn main()
{
    let args: Vec<_> = std::env::args_os().collect();
    mac_build::package::build(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources.json"),
                              std::path::Path::new(&args[1]),
                              std::path::Path::new(&args[2])).expect("package private SDK fixture");
    use mac_formats::{macbinary::{self, MacFile},
                      resource::{self, Resource}};
    let output = std::path::Path::new(&args[2]);
    if let Some(system) = args.get(3)
    {
        let mut file = macbinary::decode(&std::fs::read(system).expect("read current System package")).expect("decode current System package");
        let mut resources =
            resource::decode(&file.resources).expect("decode current System resources");
        resources.iter_mut()
                 .find(|resource| resource.kind == *b"DLOG" && resource.id == -6042)
                 .expect("System Open layout")
                 .data
                 .clear();
        file.resources =
            resource::encode_native(&resources).expect("encode malformed-layout System resources");
        std::fs::write(output.join("Malformed System.bin"), macbinary::encode(&file).expect("encode private System package"))
            .expect("write malformed-layout System package");
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
    let malformed = resource::encode_native(&malformed).expect("encode malformed relocation fixture");
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
