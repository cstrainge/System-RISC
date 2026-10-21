//! Real FSYS DRVR and resident FORK broker, exercised solely through Toolbox.
#![forbid(unsafe_code)]
use mac_toolbox::{Cnid, Error, System, VolumeId,
                  files::{CatalogInfo, DataFileSpec, Filesystem, Fork, ROOT_ID, SeekFrom}};
const NESTED: &[u8] = include_bytes!("../../mac-sit/tests/fixtures/nested-v1.sit");
const FORKS: &[u8] = include_bytes!("../../mac-sit/tests/fixtures/forks-v1.sit");
fn require(value: bool) -> Result<(), Error>
{
    if value { Ok(()) } else { Err(Error::BadAbi) }
}
fn create(system: &System<'_>, volume: VolumeId, name: &str, bytes: &[u8])
          -> Result<CatalogInfo, Error>
{
    let mut writer = system.files().create_data(DataFileSpec { volume,
                                                                parent: ROOT_ID,
                                                                name,
                                                                replacing: None,
                                                                file_type: (*b"SIT!").into(),
                                                                creator: (*b"SIT!").into() },
                                                 bytes.len())?;
    writer.write_all(bytes)?;
    writer.finish()
}
pub fn check(system: &System<'_>) -> Result<(), Error>
{
    let files = system.files();
    let startup = files.volume_at(0)?.id;
    system.log("STUFFIT_STAGE create\n")?;
    let archive = create(system, startup, "Examples.sit", NESTED)?;
    let forked = create(system, startup, "Forked.sit", FORKS)?;
    let duplicate = create(system, startup, "Copy.sit", NESTED)?;
    let mut public = files.open(startup, archive.id, Fork::Data)?;
    public.seek(SeekFrom::Start(17))?;
    system.log("STUFFIT_STAGE mount\n")?;
    let mount = files.mount_file(startup, archive.id)?;
    let volume = mount.id;
    require(mount.read_only && mount.name() == "Examples.sit" && mount.capacity.is_none())?;
    require(mount.filesystem
            == Filesystem::Driver { kind:    (*b"SIT ").into(),
                                    version: 1 })?;
    require(files.mount_file(startup, archive.id)?.id == volume)?;
    require(public.position()? == 17)?;
    public.close()?;
    let other = files.mount_file(startup, duplicate.id)?.id;
    require(other != volume)?;
    system.log("STUFFIT_STAGE catalog\n")?;
    let top = files.child(volume, ROOT_ID, 0)?;
    require(top.name() == "Top" && top.is_directory() && top.child_count == Some(3))?;
    let cafe = files.lookup(volume, top.id, "CAFE\u{301}")?;
    require(cafe.name() == "Café" && cafe.file_type == (*b"TEXT").into())?;
    let pattern = files.lookup(volume, ROOT_ID, "Top/Sub/Pattern")?;
    require(pattern.data_length == 8192 && pattern.resource_length == 0)?;
    require(files.rename(volume, pattern.id, "Renamed") == Err(Error::ReadOnly))?;
    require(matches!(files.create_data(DataFileSpec { volume,
                                                      parent: ROOT_ID,
                                                      name: "Refused",
                                                      replacing: None,
                                                      file_type: (*b"TEXT").into(),
                                                      creator: (*b"TEST").into() },
                                       0),
                     Err(Error::ReadOnly)))?;
    system.log("STUFFIT_STAGE reads\n")?;
    let mut file = files.open(volume, pattern.id, Fork::Data)?;
    let mut bytes = [0; 4096];
    for _ in 0..2
    {
        require(file.read(&mut bytes)? == bytes.len())?;
        for (i, byte) in bytes.iter().enumerate()
        {
            require(*byte == i as u8)?;
        }
    }
    require(file.read(&mut bytes[..1])? == 0)?;
    require(file.seek(SeekFrom::Start(7))? == 7)?;
    require(file.read(&mut bytes[..23])? == 23)?;
    for (i, byte) in bytes[..23].iter().enumerate()
    {
        require(*byte == (i + 7) as u8)?;
    }
    require(files.unmount(volume) == Err(Error::ServiceBusy))?;
    file.close()?;
    system.log("STUFFIT_STAGE copy-out\n")?;
    let mut source = files.open(volume, cafe.id, Fork::Data)?;
    let n = source.read(&mut bytes)?;
    require(&bytes[..n] == b"Native MacRoman name\r")?;
    source.close()?;
    let extracted = create(system, startup, "Extracted", &bytes[..n])?;
    let mut data = files.open(startup, extracted.id, Fork::Data)?;
    require(data.read(&mut bytes)? == n && &bytes[..n] == b"Native MacRoman name\r")?;
    data.close()?;
    system.log("STUFFIT_STAGE forks\n")?;
    let forks = files.mount_file(startup, forked.id)?.id;
    let item = files.child(forks, ROOT_ID, 0)?;
    require(item.name() == "Forked"
            && item.file_type == (*b"APPL").into()
            && item.data_length == 10
            && item.resource_length == 2048)?;
    let mut resource = files.open(forks, item.id, Fork::Resource)?;
    require(resource.read(&mut bytes)? == 2048)?;
    for (i, byte) in bytes[..2048].iter().enumerate()
    {
        require(*byte == i as u8)?;
    }
    resource.close()?;
    let mut data = files.open(forks, item.id, Fork::Data)?;
    require(data.read(&mut bytes)? == 10 && &bytes[..10] == b"Data fork\r")?;
    data.close()?;
    system.log("STUFFIT_STAGE corruption\n")?;
    let mut corrupt = [0; FORKS.len()];
    corrupt.copy_from_slice(FORKS);
    corrupt[134] ^= 1;
    let bad = create(system, startup, "Bad.sit", &corrupt)?;
    let badvol = files.mount_file(startup, bad.id)?.id;
    let baditem = files.child(badvol, ROOT_ID, 0)?;
    let mut badfile = files.open(badvol, baditem.id, Fork::Resource)?;
    require(badfile.read(&mut bytes) == Err(Error::Io))?;
    badfile.close()?;
    files.unmount(badvol)?;
    let bad = create(system, startup, "Not classic.sit", b"not an archive")?;
    require(matches!(files.mount_file(startup, bad.id), Err(Error::Io)))?;
    system.log("STUFFIT_STAGE stale\n")?;
    files.unmount(volume)?;
    let new = files.mount_file(startup, archive.id)?.id;
    require(new != volume && (new.0 & 0xffff) == (volume.0 & 0xffff))?;
    require(files.child(volume, Cnid(2), 0) == Err(Error::NotFound))?;
    files.unmount(new)?;
    files.unmount(other)?;
    files.unmount(forks)?;
    // Kernel-owned source marks must disappear after acknowledged teardown.
    files.rename(startup, archive.id, "Released.sit")?;
    Ok(())
}
