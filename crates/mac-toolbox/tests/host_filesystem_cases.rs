//! Canonical native mount assertions, compiled into the private userspace app.
use mac_toolbox::{Error, System, VolumeId, files::Filesystem};
fn require(value: bool) -> Result<(), Error>
{
    if value { Ok(()) } else { Err(Error::BadAbi) }
}
pub fn check(system: &System<'_>) -> Result<(), Error>
{
    let files = system.files();
    let before = files.volume_count()?;
    system.log("HOST_FS_STAGE exports\n")?;
    require(files.host_export_count()? == 2)?;
    system.log("HOST_FS_STAGE mount-rw\n")?;
    let writable = files.mount_host_export(0)?;
    require(writable.source == mac_toolbox::files::VolumeSource::Filesystem)?;
    require(writable.name() == "Native Host"
            && !writable.read_only
            && writable.capacity.is_none())?;
    require(writable.filesystem
            == Filesystem::Driver { kind:    (*b"HOST").into(),
                                    version: 1 })?;
    require(files.volume_count()? == before + 1)?;
    system.log("HOST_FS_STAGE catalog\n")?;
    let root = files.catalog(writable.id, mac_toolbox::files::ROOT_ID)?;
    require(root.is_directory() && root.name() == "Native Host" && root.child_count == 2)?;
    require(files.parent(writable.id, root.id)? == root)?;
    let first = files.child(writable.id, root.id, 0)?;
    let second = files.child(writable.id, root.id, 1)?;
    require(first.id != second.id && first.parent == root.id && second.parent == root.id)?;
    let (folder, file) = if first.is_directory() { (first, second) } else { (second, first) };
    require(folder.is_directory() && folder.name() == "Tools")?;
    require(!file.is_directory()
            && file.name() == "Café"
            && file.data_length == 23
            && file.resource_length == 0)?;
    require(files.catalog(writable.id, file.id)? == file)?;
    require(files.parent(writable.id, file.id)? == root)?;
    require(files.child(writable.id, root.id, 2) == Err(Error::NotFound))?;
    require(files.child(writable.id, file.id, 0) == Err(Error::BadArgument))?;
    let exact_folder = files.catalog(writable.id, folder.id)?;
    require(exact_folder.child_count == 1)?;
    let nested = files.child(writable.id, folder.id, 0)?;
    require(nested.name() == "Child" && nested.parent == folder.id && nested.data_length == 15)?;
    require(files.parent(writable.id, nested.id)? == exact_folder)?;
    require(files.volume(writable.id)? == writable)?;
    require(files.mount_host_export(0)?.id == writable.id)?;
    require(files.volume_count()? == before + 1)?;
    system.log("HOST_FS_STAGE invalid-export\n")?;
    require(files.mount_host_export(2) == Err(Error::NotFound))?;
    require(files.volume_count()? == before + 1)?;
    system.log("HOST_FS_STAGE mount-ro\n")?;
    let read_only = files.mount_host_export(1)?;
    require(read_only.name() == "Read Only Host"
            && read_only.read_only
            && read_only.capacity.is_none())?;
    require(files.volume_count()? == before + 2)?;
    require(files.catalog(read_only.id, mac_toolbox::files::ROOT_ID)?.child_count == 0)?;
    require(files.child(read_only.id, mac_toolbox::files::ROOT_ID, 0) == Err(Error::NotFound))?;
    require(files.new_folder(read_only.id, mac_toolbox::files::ROOT_ID, "Forbidden")
            == Err(Error::ReadOnly))?;
    let write = files.create_data(mac_toolbox::files::DataFileSpec { volume:    read_only.id,
                                                                parent:
                                                                    mac_toolbox::files::ROOT_ID,
                                                                name:      "Forbidden",
                                                                replacing: None,
                                                                file_type: (*b"TEXT").into(),
                                                                creator:   (*b"TEST").into() },
                             0);
    require(matches!(write, Err(Error::ReadOnly)))?;
    system.log("HOST_FS_STAGE unmount-and-reuse\n")?;
    files.unmount(writable.id)?;
    require(files.volume(writable.id) == Err(Error::NotFound))?;
    require(files.catalog(writable.id, root.id) == Err(Error::NotFound))?;
    let replacement = files.mount_host_export(0)?;
    require(replacement.id != writable.id && replacement.id.0 & 0xffff == writable.id.0 & 0xffff)?;
    require(files.volume(writable.id) == Err(Error::NotFound))?;
    require(files.child(writable.id, root.id, 0) == Err(Error::NotFound))?;
    require(files.unmount(writable.id) == Err(Error::NotFound))?;
    require(files.unmount(VolumeId(replacement.id.0 ^ 0x1000_0000)) == Err(Error::NotFound))?;
    files.unmount(replacement.id)?;
    files.unmount(read_only.id)?;
    require(files.volume_count()? == before)?;
    Ok(())
}
