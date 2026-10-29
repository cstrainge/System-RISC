//! Private fixture declaration staging; normal linked-contract validation stays active.
use std::{env, fs,
          path::{Path, PathBuf}};
pub fn prepare(root: &Path, stage: &Path) -> PathBuf
{
    let identity =
        env::var("SYSTEM7_PRESSURE_IDENTITY").unwrap_or_else(|_| {
                                                 "7731a00000000000000000000000aa01".into()
                                             });
    let name = env::var("SYSTEM7_PRESSURE_NAME").unwrap_or_else(|_| "Pressure Holder 00".into());
    assert!(identity.len() == 32 && identity.bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b' '));
    fs::create_dir_all(stage).expect("stage pressure declaration");
    // Manifest::read resolves the actual package version through the adjacent
    // Cargo manifest. A link preserves its real canonical package/workspace;
    // copying it would invent a different crate rooted in this generated view.
    let cargo = stage.join("Cargo.toml");
    match fs::symlink_metadata(&cargo)
    {
        Ok(metadata) =>
        {
            assert!(metadata.file_type().is_symlink(),
                    "pressure staging owns only a generated Cargo manifest link");
            fs::remove_file(&cargo).expect("replace generated pressure Cargo manifest link");
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => panic!("inspect pressure Cargo manifest link: {error}")
    }
    link_manifest(&root.join("Cargo.toml")
                       .canonicalize()
                       .expect("canonical fixture Cargo manifest"),
                  &cargo).expect("link genuine pressure fixture Cargo manifest; pressure \
                                  preparation requires host symlink permission");
    for (source, destination) in
        [("module.pressure.json", "module.json"), ("resources.pressure.json", "resources.json")]
    {
        let text = fs::read_to_string(root.join(source)).expect("read pressure declaration \
                                                                 template")
                                                        .replace("@IDENTITY@", &identity)
                                                        .replace("@NAME@", &name);
        fs::write(stage.join(destination), text).expect("write pressure declaration");
    }
    stage.join("resources.json")
}

// This is host artifact preparation only. The native guest uses no alternate
// services. Pressure runs currently use Linux; Windows needs symlink privilege.
fn link_manifest(source: &Path, destination: &Path) -> std::io::Result<()>
{
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source, destination)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(source, destination)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (source, destination);
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported,
                                "pressure fixture preparation requires a host file-symlink \
                                 facility"))
    }
}
