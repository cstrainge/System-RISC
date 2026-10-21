//! Canonical backing-file mount assertions through the public userspace SDK.
#![forbid(unsafe_code)]
use mac_toolbox::{Cnid, Error, System, VolumeId,
                  files::{CatalogInfo, DataFileSpec, Filesystem, Fork, ROOT_ID, SeekFrom}};
fn require(value: bool) -> Result<(), Error>
{
    if value { Ok(()) } else { Err(Error::BadAbi) }
}
#[inline(never)]
fn create(system: &System<'_>, spec: DataFileSpec<'_>, length: usize)
          -> Result<CatalogInfo, Error>
{
    let mut writer = system.files().create_data(spec, length)?;
    let mut bytes = [0; 8192];
    let mut at = 0;
    while at < length
    {
        let count = (length - at).min(bytes.len());
        for (i, byte) in bytes[..count].iter_mut().enumerate()
        {
            *byte = ((at + i) * 31 + 7) as u8;
        }
        writer.write_all(&bytes[..count])?;
        at += count;
    }
    writer.finish()
}
#[inline(never)]
fn read(system: &System<'_>, volume: VolumeId) -> Result<(), Error>
{
    let files = system.files();
    let child = files.child(volume, ROOT_ID, 0)?;
    require(child.id == Cnid(16) && child.name() == "Payload" && child.data_length == 131089)?;
    require(files.child(volume, ROOT_ID, 1) == Err(Error::NotFound))?;
    let mut file = files.open(volume, child.id, Fork::Data)?;
    let mut bytes = [0; 65536];
    require(file.read(&mut bytes)? == bytes.len())?;
    for (i, byte) in bytes.iter().enumerate()
    {
        require(*byte == (i * 31 + 7) as u8)?;
    }
    require(files.unmount(volume) == Err(Error::ServiceBusy))?;
    require(file.seek(SeekFrom::Start(65530))? == 65530)?;
    require(file.read(&mut bytes[..37])? == 37)?;
    for (i, byte) in bytes[..37].iter().enumerate()
    {
        require(*byte == ((65530 + i) * 31 + 7) as u8)?;
    }
    require(file.seek(SeekFrom::End(-17))? == 131072)?;
    require(file.read(&mut bytes[..100])? == 17)?;
    for (i, byte) in bytes[..17].iter().enumerate()
    {
        require(*byte == ((131072 + i) * 31 + 7) as u8)?;
    }
    require(file.read(&mut bytes[..1])? == 0)?;
    file.close()?;
    let mut resource = files.open(volume, child.id, Fork::Resource)?;
    require(resource.len()? == 0 && resource.read(&mut bytes[..1])? == 0)?;
    resource.close()
}
pub fn check(system: &System<'_>) -> Result<(), Error>
{
    let files = system.files();
    let startup = files.volume_at(0)?.id;
    let before = files.volume_count()?;
    let spec = DataFileSpec { volume:    startup,
                              parent:    ROOT_ID,
                              name:      "Archive A",
                              replacing: None,
                              file_type: (*b"BINA").into(),
                              creator:   (*b"TEST").into() };
    system.log("FILE_SOURCE_STAGE create\n")?;
    let original = create(system, spec, 131089)?;
    let copy = create(system,
                      DataFileSpec { name: "Archive B",
                                     ..spec },
                      131089)?;
    let bad = create(system,
                     DataFileSpec { name: "Bad Archive",
                                    ..spec },
                     3)?;
    require(original.id != copy.id)?;
    system.log("FILE_SOURCE_STAGE mount\n")?;
    let mut public = files.open(startup, original.id, Fork::Data)?;
    public.seek(SeekFrom::Start(17))?;
    let mounted = files.mount_file(startup, original.id)?;
    require(mounted.read_only && mounted.capacity.is_none())?;
    require(mounted.filesystem
            == Filesystem::Driver { kind:    (*b"TEST").into(),
                                    version: 1 })?;
    require(files.mount_file(startup, original.id)?.id == mounted.id)?;
    let second = files.mount_file(startup, copy.id)?;
    require(second.id != mounted.id && files.volume_count()? == before + 2)?;
    system.log("FILE_SOURCE_STAGE positioned-read\n")?;
    read(system, mounted.id)?;
    require(public.position()? == 17)?;
    public.close()?;
    // The kernel-owned source lease outlives the application's own fork mark.
    let mut blocked = files.create_data(DataFileSpec { replacing: Some(original.id),
                                                       ..spec },
                                        0)?;
    require(blocked.finish() == Err(Error::ServiceBusy))?;
    drop(blocked);
    system.log("FILE_SOURCE_STAGE invalid-cleanup\n")?;
    require(files.mount_file(startup, bad.id) == Err(Error::BadArgument))?;
    require(files.volume_count()? == before + 2)?;
    let mut replace_bad = files.create_data(DataFileSpec { name: "Bad Archive",
                                                           replacing: Some(bad.id),
                                                           ..spec },
                                            0)?;
    replace_bad.finish()?;
    require(files.mount_file(VolumeId(startup.0 ^ 0x1000_0000), original.id)
            == Err(Error::NotFound))?;
    system.log("FILE_SOURCE_STAGE reuse\n")?;
    files.unmount(mounted.id)?;
    let reused = files.mount_file(startup, original.id)?;
    require(reused.id != mounted.id && reused.id.0 & 0xffff == mounted.id.0 & 0xffff)?;
    require(files.volume(mounted.id) == Err(Error::NotFound))?;
    require(files.catalog(mounted.id, ROOT_ID) == Err(Error::NotFound))?;
    read(system, reused.id)?;
    require(files.new_folder(reused.id, ROOT_ID, "Refused") == Err(Error::ReadOnly))?;
    files.unmount(reused.id)?;
    files.unmount(second.id)?;
    require(files.volume_count()? == before)?;
    system.log("FILE_SOURCE_STAGE lease-released\n")?;
    let mut replace = files.create_data(DataFileSpec { replacing: Some(original.id),
                                                       ..spec },
                                        0)?;
    require(replace.finish()?.id == original.id)?;
    Ok(())
}
