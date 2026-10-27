fn main()
{
    let args: Vec<_> = std::env::args_os().collect();
    let output = std::path::Path::new(&args[2]);
    mac_build::package::build(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources.json"),
        std::path::Path::new(&args[1]),output).expect("package capacity worker");
    let bytes = std::fs::read(output.join("CapacityWorker.bin")).unwrap();
    let mut file = mac_formats::macbinary::decode(&bytes).unwrap();
    for index in 0..9
    {
        file.name = format!("Capacity {index}").into_bytes();
        std::fs::write(output.join(format!("Capacity {index}.bin")),
                       mac_formats::macbinary::encode(&file).unwrap()).unwrap();
    }
}
