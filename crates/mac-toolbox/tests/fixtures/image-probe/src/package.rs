fn main()
{
    let args: Vec<_> = std::env::args_os().collect();
    let output = std::path::Path::new(&args[2]);
    mac_build::package::build(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources.json"),
        std::path::Path::new(&args[1]), output).expect("package private image probe");
    let bytes = std::fs::read(output.join("ImageProbe.bin")).expect("read probe package");
    let mut file = mac_formats::macbinary::decode(&bytes).expect("decode probe package");
    let system = mac_formats::resource::decode(&std::fs::read(&args[3]).expect("read System fork"))
        .expect("decode System fork");
    let mut resources = mac_formats::resource::decode(&file.resources).expect("decode probe fork");
    let definition = system.into_iter()
                           .find(|r| r.kind == *b"CDEF" && r.id == -32759)
                           .expect("current System supplies the Static Text CDEF");
    resources.push(definition);
    file.resources = mac_formats::resource::encode(&resources).expect("encode probe definition");
    std::fs::write(output.join("ImageProbe.bin"), mac_formats::macbinary::encode(&file)
        .expect("encode base probe")).expect("write base probe");
    for index in 0..16
    {
        file.name = format!("ImageProbe {index:02}").into_bytes();
        let bytes = mac_formats::macbinary::encode(&file).expect("encode probe copy");
        std::fs::write(output.join(format!("ImageProbe {index:02}.bin")), bytes).expect("write probe copy");
    }
}
