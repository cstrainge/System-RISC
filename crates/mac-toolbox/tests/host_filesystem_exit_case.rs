//! A real second application deliberately exits with two live provider marks.
use mac_toolbox::{Error, System,
                  files::{Fork, ROOT_ID}};
fn require(value: bool) -> Result<(), Error>
{
    if value { Ok(()) } else { Err(Error::BadAbi) }
}
pub fn check(system: &System<'_>) -> Result<(), Error>
{
    let files = system.files();
    let mut mounted = None;
    for index in 0..files.volume_count()?
    {
        let info = files.volume_at(index)?;
        if info.name() == "Native Host"
        {
            mounted = Some(info.id);
            break;
        }
    }
    let volume = mounted.ok_or(Error::NotFound)?;
    let folder = files.lookup(volume, ROOT_ID, "Tools")?;
    let file = files.lookup(volume, folder.id, "Forks")?;
    let mut data = files.open(volume, file.id, Fork::Data)?;
    let mut resources = files.open(volume, file.id, Fork::Resource)?;
    let mut bytes = [0; 3];
    require(data.read(&mut bytes)? == 3 && bytes == [7, 38, 69])?;
    require(resources.read(&mut bytes)? == 3 && bytes == [11, 24, 37])?;
    require(files.unmount(volume) == Err(Error::ServiceBusy))?;
    let mut writer =
        files.create_data(mac_toolbox::files::DataFileSpec { volume,
                                                             parent: ROOT_ID,
                                                             name: "Exit staging",
                                                             replacing: None,
                                                             file_type: (*b"TEXT").into(),
                                                             creator: (*b"TEST").into() },
                          3)?;
    writer.write_all(b"abc")?;
    core::mem::forget(writer);
    core::mem::forget(files.watch_directory(volume, ROOT_ID)?);
    core::mem::forget(data);
    core::mem::forget(resources);
    system.log("HOST_FS_EXIT_WORKER_READY\n")?;
    // Returning enters the real scheduler/process EXIT path, bypassing Drop.
    Ok(())
}
