//! Ordinary std application; behavioral assertions use the public SDK.
use mac_toolbox::System;
#[path = "../../../finder_recovery_cases.rs"]
pub mod cases;
fn main() -> Result<(), Box<dyn std::error::Error>>
{
    let system = System::new()?;
    let files = system.files();
    let mut documents = None;
    for index in 0..files.volume_count()?
    {
        let volume = files.volume_at(index)?;
        if volume.startup
        {
            assert!(documents.is_none(), "multiple startup volumes reached Finder author code");
            documents = Some(std::path::PathBuf::from(format!("/Volumes/{}/Documents",
                                                              volume.name.as_str())));
        }
    }
    assert_eq!(Some(std::env::current_dir()?),
               documents,
               "Finder author started without the startup user's Documents directory");
    system.log("FINDER_RECOVERY_DOCUMENTS_OBSERVED\n")?;
    match cases::finder(&system)
    {
        Ok(()) => Ok(()),
        Err(error) =>
        {
            let _ = system.log("FINDER_RECOVERY_FAIL service\n");
            Err(error.into())
        }
    }
}
