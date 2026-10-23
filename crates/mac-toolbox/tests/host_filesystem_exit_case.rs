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
    let own = system.processes().current()?;
    let own_volume = files.volume(own.volume)?;
    require(!own_volume.startup && own.name() == "Host Exit Worker")?;
    let mut text = [0; 64];
    require(system.resources().application().string(2000, &mut text)?
            == "external-application-resource")?;
    system.log(if own_volume.read_only
               {
                   "HOST_FS_EXTERNAL_APPLICATION_READY_RO\n"
               }
               else
               {
                   "HOST_FS_EXTERNAL_APPLICATION_READY_RW\n"
               })?;
    let mut released = false;
    for _ in 0..300
    {
        if matches!(system.events().wait_event(mac_toolbox::EventMask::KEY_DOWN, 1)?,
                    mac_toolbox::Event::KeyDown { .. })
        {
            released = true;
            break;
        }
    }
    require(released)?;
    if own_volume.read_only
    {
        system.log("HOST_FS_EXTERNAL_APPLICATION_EXIT_RO\n")?;
        return Ok(());
    }
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
    let startup = files.volume_at(0)?.id;
    if let Ok(source) = files.lookup(startup, ROOT_ID, "Exit Transfer Source")
    {
        use mac_toolbox::files::*;
        let target = files.lookup(volume, ROOT_ID, "Exit Transfer Target")?;
        let mut owner =
            files.begin_transfer(TransferSpec { mode:        TransferMode::Copy,
                                                destination:
                                                    TransferDestination::Folder { volume,
                                                                                  parent:
                                                                                      target.id } },
                                 &[TransferSource::new(startup, source.id, ROOT_ID)])?;
        for _ in 0..10000
        {
            let progress = owner.step().map_err(|failure| failure.cause)?;
            require(!progress.finished())?;
            if progress.completed_bytes != 0
            {
                core::mem::forget(owner);
                system.log("HOST_FS_EXIT_WORKER_READY\n")?;
                return Ok(());
            }
            system.events().wait_event(mac_toolbox::EventMask::ALL, 1)?;
        }
        return Err(Error::ServiceBusy);
    }
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
