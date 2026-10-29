#[path = "../scheduler_manifest.rs"]
mod scheduler_manifest;
#[path = "../pressure_manifest.rs"]
mod pressure_manifest;
#[path = "../smp_manifest.rs"]
mod smp_manifest;
fn main()
{
    let args: Vec<_> = std::env::args_os().collect();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = std::path::Path::new(&args[2]);
    let manifest = if cfg!(feature = "smp")
    {
        smp_manifest::prepare(root, &output.join("smp-declaration"))
    }
    else if cfg!(feature = "scheduler")
    {
        scheduler_manifest::prepare(root, &output.join("scheduler-declaration"))
    }
    else if cfg!(feature = "pressure")
    {
        pressure_manifest::prepare(root, &output.join("declaration"))
    }
    else
    {
        root.join(if cfg!(feature = "workers") && root.ends_with("driver-client")
                  {
                      "resources.workers.json"
                  }
                  else
                  {
                      "resources.json"
                  })
    };
    mac_build::package::build(&manifest, std::path::Path::new(&args[1]), output).unwrap();
    if cfg!(feature = "pressure")
    {
        let manifest = mac_build::Manifest::read(&manifest).unwrap();
        let file = mac_formats::macbinary::decode(&std::fs::read(output.join(format!("{}.bin", manifest.name))).unwrap()).unwrap();
        let resources = mac_formats::resource::decode(&file.resources).unwrap();
        let package = mac_formats::module::Package::inspect(&resources, file.file_type).unwrap();
        let bytes = package.code.descriptor.load_limit - package.code.descriptor.load_base;
        std::fs::write(output.join("module-load-bytes.txt"), format!("{bytes}\n")).unwrap();
    }
}
