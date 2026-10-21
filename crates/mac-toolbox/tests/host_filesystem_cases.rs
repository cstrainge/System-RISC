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
    system.log("HOST_FS_STAGE cursor-scan\n")?;
    let mut prior_scan = check_scan(system, writable.id)?;
    system.log("HOST_FS_STAGE catalog\n")?;
    let root = files.catalog(writable.id, mac_toolbox::files::ROOT_ID)?;
    require(root.is_directory() && root.name() == "Native Host" && root.child_count == Some(2))?;
    require(files.parent(writable.id, root.id)? == root)?;
    let first = files.child(writable.id, root.id, 0)?;
    let second = files.child(writable.id, root.id, 1)?;
    require(first.id != second.id && first.parent == root.id && second.parent == root.id)?;
    let (folder, file) = if first.is_directory() { (first, second) } else { (second, first) };
    require(folder.is_directory() && folder.name() == "Tools" && folder.child_count.is_none())?;
    require(!file.is_directory()
            && file.name() == "Café"
            && file.data_length == 23
            && file.resource_length == 0)?;
    require(files.catalog(writable.id, file.id)? == file)?;
    require(files.parent(writable.id, file.id)? == root)?;
    require(files.child(writable.id, root.id, 2) == Err(Error::NotFound))?;
    require(files.child(writable.id, file.id, 0) == Err(Error::BadArgument))?;
    let exact_folder = files.catalog(writable.id, folder.id)?;
    require(files.allocation_info(writable.id, file.id)?.total_bytes()?.is_none())?;
    require(files.allocation_info(writable.id, folder.id)?.total_bytes()? == Some(0))?;
    require(exact_folder.child_count == Some(2))?;
    let a = files.child(writable.id, folder.id, 0)?;
    let b = files.child(writable.id, folder.id, 1)?;
    let (nested, forks) = if a.name() == "Child" { (a, b) } else { (b, a) };
    require(nested.name() == "Child" && nested.parent == folder.id && nested.data_length == 15)?;
    require(files.parent(writable.id, nested.id)? == exact_folder)?;
    system.log("HOST_FS_STAGE paths\n")?;
    require(files.lookup(writable.id, root.id, "//tools/./Child")? == nested)?;
    require(files.lookup(writable.id, folder.id, "../CAFE\u{301}")? == file)?;
    require(files.lookup(writable.id, nested.id, "")? == nested)?;
    require(files.lookup(writable.id, folder.id, "../../")? == root)?;
    require(files.lookup(writable.id, folder.id, ".")? == exact_folder)?;
    require(files.lookup(writable.id, root.id, "/Tools/")? == exact_folder)?;
    require(files.lookup(writable.id, nested.id, ".") == Err(Error::BadArgument))?;
    require(files.lookup(writable.id, root.id, "/Café/") == Err(Error::BadArgument))?;
    require(files.lookup(writable.id, root.id, "Missing") == Err(Error::NotFound))?;
    expect_path(system, writable.id, root.id, "/")?;
    expect_path(system, writable.id, file.id, "/Café")?;
    expect_path(system, writable.id, nested.id, "/Tools/Child")?;
    let mut small = [0xa5; 2];
    let mut call = mac_toolbox::abi::files::FileCall { operation: mac_toolbox::abi::files::PATH,
                                                       volume: writable.id.0,
                                                       cnid: nested.id.0,
                                                       ..Default::default() };
    require(system.client().file(&mut call, "", &mut small) == Err(Error::NoMemory)
            && small == [0xa5; 2])?;
    require(forks.name() == "Forks"
            && forks.data_length == 131089
            && forks.resource_length == 70003)?;
    system.log("HOST_FS_STAGE fork-io\n")?;
    check_forks(system, writable.id, forks.id)?;
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
    require(files.catalog(read_only.id, mac_toolbox::files::ROOT_ID)?.child_count == Some(0))?;
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
    check_exit_cleanup(system, writable.id)?;
    require(files.volume(writable.id) == Err(Error::NotFound))?;
    require(files.catalog(writable.id, root.id) == Err(Error::NotFound))?;
    let replacement = files.mount_host_export(0)?;
    require(files.scan_directory(replacement.id, mac_toolbox::files::ROOT_ID, &mut prior_scan)
            == Err(Error::BadArgument))?;
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

fn check_scan(system: &System<'_>, volume: VolumeId)
              -> Result<mac_toolbox::files::DirectoryCursor, Error>
{
    use mac_toolbox::files::{DirectoryCursor, ROOT_ID, ScanStep};
    let mut cursor = DirectoryCursor::default();
    let mut count = 0;
    for _ in 0..128
    {
        match system.files().scan_directory(volume, ROOT_ID, &mut cursor)?
        {
            ScanStep::Entry { catalog,
                              data_allocated,
                              resource_allocated } =>
            {
                require(catalog.parent == ROOT_ID && catalog.volume == volume)?;
                if catalog.is_directory()
                {
                    require(catalog.name() == "Tools"
                            && data_allocated == Some(0)
                            && resource_allocated == Some(0))?;
                }
                else
                {
                    require(catalog.name() == "Café"
                            && data_allocated.is_none()
                            && resource_allocated.is_none())?;
                }
                count += 1;
            }
            ScanStep::Pending =>
            {
                system.events().wait_event(mac_toolbox::EventMask::ALL, 1)?;
            }
            ScanStep::Done =>
            {
                require(count == 2)?;
                require(matches!(system.files().scan_directory(volume, ROOT_ID, &mut cursor)?,
                                 ScanStep::Done))?;
                return Ok(cursor);
            }
        }
    }
    Err(Error::ServiceBusy)
}

fn expect_path(system: &System<'_>, volume: VolumeId, node: mac_toolbox::Cnid, expected: &str)
               -> Result<(), Error>
{
    struct Text
    {
        bytes: [u8; 256],
        len:   usize
    }
    impl core::fmt::Write for Text
    {
        fn write_str(&mut self, text: &str) -> core::fmt::Result
        {
            let end = self.len.checked_add(text.len()).ok_or(core::fmt::Error)?;
            self.bytes
                .get_mut(self.len..end)
                .ok_or(core::fmt::Error)?
                .copy_from_slice(text.as_bytes());
            self.len = end;
            Ok(())
        }
    }
    let mut text = Text { bytes: [0; 256],
                          len:   0 };
    system.files().path(volume, node, &mut text)?;
    require(&text.bytes[..text.len] == expected.as_bytes())
}

fn check_forks(system: &System<'_>, volume: VolumeId, node: mac_toolbox::Cnid)
               -> Result<(), Error>
{
    use mac_toolbox::files::{Fork, SeekFrom};
    let files = system.files();
    let mut data = files.open(volume, node, Fork::Data)?;
    let mut resource = files.open(volume, node, Fork::Resource)?;
    require(data.len()? == 131089 && resource.len()? == 70003)?;
    require(files.unmount(volume) == Err(Error::ServiceBusy))?;
    require(data.read(&mut [])? == 0 && data.position()? == 0)?;
    let mut buffer = [0u8; 4096];
    for (fork, length, factor, bias) in
        [(&mut data, 131089usize, 31usize, 7usize), (&mut resource, 70003, 13, 11)]
    {
        let mut at = 0;
        while at < length
        {
            let count = fork.read(&mut buffer)?;
            require(count != 0 && count <= length - at)?;
            for (i, byte) in buffer[..count].iter().enumerate()
            {
                require(*byte == ((at + i) * factor + bias) as u8)?;
            }
            at += count;
        }
        require(fork.position()? == length as u64 && fork.read(&mut buffer)? == 0)?;
    }
    require(data.seek(SeekFrom::End(-17))? == 131072)?;
    require(data.read(&mut buffer)? == 17)?;
    require(resource.seek(SeekFrom::Start(10))? == 10)?;
    require(resource.seek(SeekFrom::Current(-5))? == 5)?;
    require(resource.read(&mut buffer[..3])? == 3 && buffer[..3] == [76, 89, 102])?;
    require(data.seek(SeekFrom::End(1)) == Err(Error::BadArgument))?;
    data.close()?;
    resource.close()?;
    // Exercise generation checks through the SDK's explicit wire escape hatch.
    // Owned wrappers deliberately do not expose their raw mark identities.
    use mac_toolbox::abi::files::*;
    let client = system.client();
    let mut opened = FileCall { operation: OPEN,
                                volume: volume.0,
                                cnid: node.0,
                                ..Default::default() };
    client.file(&mut opened, "", &mut [])?;
    let stale = opened.handle;
    client.file(&mut FileCall { operation: CLOSE,
                                handle: stale,
                                ..Default::default() },
                "",
                &mut [])?;
    opened = FileCall { operation: OPEN,
                        volume: volume.0,
                        cnid: node.0,
                        ..Default::default() };
    client.file(&mut opened, "", &mut [])?;
    require(opened.handle != stale && opened.handle & 0xffff == stale & 0xffff)?;
    for operation in [READ, TELL, SEEK, CLOSE]
    {
        require(client.file(&mut FileCall { operation,
                                            handle: stale,
                                            ..Default::default() },
                            "",
                            &mut [])
                == Err(Error::NotFound))?;
    }
    require(client.file(&mut FileCall { operation: CLOSE,
                                        handle: opened.handle ^ 0x1000_0000,
                                        ..Default::default() },
                        "",
                        &mut [])
            == Err(Error::NotFound))?;
    client.file(&mut FileCall { operation: CLOSE,
                                handle: opened.handle,
                                ..Default::default() },
                "",
                &mut [])?;
    let dropped = files.open(volume, node, Fork::Data)?;
    require(files.unmount(volume) == Err(Error::ServiceBusy))?;
    drop(dropped);
    Ok(())
}

fn check_exit_cleanup(system: &System<'_>, volume: VolumeId) -> Result<(), Error>
{
    let files = system.files();
    let mut startup = None;
    for index in 0..files.volume_count()?
    {
        let info = files.volume_at(index)?;
        if info.startup
        {
            startup = Some(info.id);
            break;
        }
    }
    let startup = startup.ok_or(Error::NotFound)?;
    let worker =
        files.lookup(startup, mac_toolbox::files::ROOT_ID, "/Applications/Host Exit Worker")?;
    let processes = system.processes();
    let launched = processes.launch(startup, worker.id)?;
    require(launched.cnid == worker.id
            && launched.volume == startup
            && launched.thread.0 >> 16 != 0)?;
    for _ in 0..300
    {
        if !processes.is_running(startup, worker.id)?
        {
            break;
        }
        system.events().wait_event(mac_toolbox::EventMask::ALL, 1)?;
    }
    require(!processes.is_running(startup, worker.id)?)?;
    for _ in 0..300
    {
        match files.unmount(volume)
        {
            Ok(()) => return Ok(()),
            Err(Error::ServiceBusy) =>
            {
                system.events().wait_event(mac_toolbox::EventMask::ALL, 1)?;
            }
            Err(error) => return Err(error)
        }
    }
    Err(Error::ServiceBusy)
}
