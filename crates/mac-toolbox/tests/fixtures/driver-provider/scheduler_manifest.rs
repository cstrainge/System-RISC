//! Private fixture declaration staging; normal linked-contract validation stays active.
use std::{fs,
          path::{Path, PathBuf}};
pub fn prepare(root: &Path, stage: &Path) -> PathBuf
{
    fs::create_dir_all(stage).expect("stage scheduler declaration");
    // Manifest::read resolves the actual package version through the adjacent
    // Cargo manifest. A link preserves its real canonical package/workspace;
    // copying it would invent a different crate rooted in this generated view.
    let cargo = stage.join("Cargo.toml");
    match fs::symlink_metadata(&cargo)
    {
        Ok(metadata) =>
        {
            assert!(metadata.file_type().is_symlink(),
                    "scheduler staging owns only a generated Cargo manifest link");
            fs::remove_file(&cargo).expect("replace generated scheduler Cargo manifest link");
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => panic!("inspect scheduler Cargo manifest link: {error}")
    }
    link_manifest(&root.join("Cargo.toml")
                       .canonicalize()
                       .expect("canonical fixture Cargo manifest"),
                  &cargo).expect("link genuine scheduler fixture Cargo manifest; scheduler \
                                  preparation requires host symlink permission");
    for (source, destination) in
        [("module.scheduler.json", "module.json"), ("resources.scheduler.json", "resources.json")]
    {
        let text =
            fs::read_to_string(root.join(source)).expect("read scheduler declaration template");
        fs::write(stage.join(destination), text).expect("write scheduler declaration");
    }
    stage.join("resources.json")
}

// This is host artifact preparation only. The native guest uses no alternate
// services. This diagnostic transport currently uses Linux; Windows needs symlink privilege.
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
                                "scheduler fixture preparation requires a host file-symlink \
                                 facility"))
    }
}
