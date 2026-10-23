fn main()
{
    let args: Vec<_> = std::env::args_os().collect();
    mac_build::package::build(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources.json"),
                              std::path::Path::new(&args[1]),
                              std::path::Path::new(&args[2])).expect("package private SDK fixture");
    use mac_formats::{macbinary::{self, MacFile},
                      resource::{self, Resource}};
    let output = std::path::Path::new(&args[2]);
    let budget=resource::encode(&[Resource {kind:*b"DATA",id:1,name:None,attributes:0,
        data:vec![0;4*1024*1024+1]}]).expect("encode valid resource fork above application payload budget");
    for (name, resources) in [("Budget App", budget), ("Corrupt App", vec![0; 16])]
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
