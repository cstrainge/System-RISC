//! Canonical native mount assertions, compiled into the private userspace app.
use mac_toolbox::{Error, System, VolumeId, files::Filesystem};
fn require(value: bool) -> Result<(), Error>
{
    if value { Ok(()) } else { Err(Error::BadAbi) }
}
/// Original string-boundary callers share the resident writer and generation
/// checks. Only this conformance fixture uses a raw Pascal-string gate.
pub fn check_legacy_writer(system: &System<'_>, gate: mac_toolbox::abi::TrapGate)
                           -> Result<(), Error>
{
    use mac_toolbox::abi::{files::*, traps::PSTRING_FILE};
    system.log("HOST_FS_STAGE legacy-write\n")?;
    let files = system.files();
    let volume = files.mount_host_export(0)?.id;
    let name = b"\x0bLegacy Caf\x8e";
    let mut call = DataWriteCall { operation: BEGIN_DATA_WRITE,
                                   volume: volume.0,
                                   parent: ROOT_ID,
                                   name: name.as_ptr(),
                                   name_length: name.len(),
                                   length: 3,
                                   when: 3_875_171_696_123_456,
                                   file_type: *b"TEXT",
                                   creator: *b"TEST",
                                   ..Default::default() };
    let invoke = |call: &mut DataWriteCall| {
        // Every pointer is a live fixture-owned buffer for this invocation;
        // the resident boundary converts text and owns bytes before parking.
        let status = unsafe {
            gate(PSTRING_FILE,
                 core::ptr::from_mut(call).cast(),
                 core::mem::size_of::<DataWriteCall>())
        };
        if status == 0 { Ok(()) } else { Err(Error::from_status(status as i32)) }
    };
    invoke(&mut call)?;
    let stale = call.handle;
    call.operation = APPEND_DATA_WRITE;
    call.data = b"abc".as_ptr();
    call.data_length = 3;
    invoke(&mut call)?;
    call.operation = COMMIT_DATA_WRITE;
    invoke(&mut call)?;
    require(&call.entry.name[..call.entry.name_length as usize] == name)?;
    let native = files.lookup(volume, mac_toolbox::files::ROOT_ID, "Legacy Café")?;
    require(native.id.0 == call.entry.id && native.data_length == 3)?;
    let mut old = DataWriteCall { operation: ABORT_DATA_WRITE,
                                  handle: stale,
                                  ..Default::default() };
    require(invoke(&mut old) == Err(Error::NotFound))?;
    check_pattern(system, volume, native.id, mac_toolbox::files::Fork::Data, 3, 1, b'a' as usize)?;
    files.unmount(volume)?;
    check_legacy_resources(system, gate)
}

fn check_legacy_resources(system: &System<'_>, gate: mac_toolbox::abi::TrapGate)
                          -> Result<(), Error>
{
    use mac_toolbox::abi::{files::*, resources::FileResourceCall, traps::PSTRING_RESOURCE};
    system.log("HOST_FS_STAGE legacy-resources\n")?;
    let files = system.files();
    let volume = files.mount_host_export(1)?.id;
    let entry = files.lookup(volume, mac_toolbox::files::ROOT_ID, "ResEdit")?;
    let mut file = FileCall { operation: OPEN,
                              volume: volume.0,
                              cnid: entry.id.0,
                              fork: RESOURCE_FORK,
                              ..Default::default() };
    system.client().file(&mut file, "", &mut [])?;
    let mut call = FileResourceCall { operation: mac_toolbox::abi::resources::COUNT_TYPES,
                                      handle: file.handle,
                                      ..Default::default() };
    // The same resident service handles original Pascal/MacRoman resource calls;
    // this fixture owns all live buffers until the trap returns.
    let status = unsafe {
        gate(PSTRING_RESOURCE,
             core::ptr::from_mut(&mut call).cast(),
             core::mem::size_of::<FileResourceCall>())
    };
    require(status == 0 && call.index > 0)?;
    let mut close = FileCall { operation: CLOSE,
                               handle: file.handle,
                               ..Default::default() };
    system.client().file(&mut close, "", &mut [])?;
    files.unmount(volume)
}
pub fn check(system: &System<'_>) -> Result<(), Error>
{
    let files = system.files();
    // System, not this fixture or Finder, mounts device exports before starting
    // userspace. Repeated explicit requests must find those same live volumes.
    system.log("HOST_FS_STAGE automatic-mounts\n")?;
    require(files.volume_count()? == 3)?;
    let automatic_rw = files.volume_at(1)?;
    let automatic_ro = files.volume_at(2)?;
    require(files.trash_directory(automatic_rw.id, false) == Err(Error::NotFound))?;
    require(files.trash_directory(automatic_ro.id, false) == Err(Error::NotFound))?;
    require(automatic_rw.name() == "Native Host" && !automatic_rw.read_only)?;
    require(automatic_ro.name() == "Read Only Host" && automatic_ro.read_only)?;
    require(files.mount_host_export(0)?.id == automatic_rw.id)?;
    require(files.mount_host_export(1)?.id == automatic_ro.id)?;
    require(files.volume_count()? == 3)?;
    files.unmount(automatic_rw.id)?;
    files.unmount(automatic_ro.id)?;
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
    require(system.resources().open_file(writable.id, file.id)?.type_count()? == 0)?;
    require(matches!(system.resources().open_file(writable.id, forks.id),
                     Err(Error::BadArgument)))?;
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
    require(files.catalog(read_only.id, mac_toolbox::files::ROOT_ID)?.child_count == Some(1))?;
    require(files.child(read_only.id, mac_toolbox::files::ROOT_ID, 1) == Err(Error::NotFound))?;
    system.log("HOST_FS_STAGE resource-inspection\n")?;
    check_resources(system, read_only.id)?;
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
    system.log("HOST_FS_STAGE native-write\n")?;
    check_writes(system, writable.id, forks.id)?;
    require(files.scan_directory(writable.id, mac_toolbox::files::ROOT_ID, &mut prior_scan)
            == Err(Error::ScanStale))?;
    system.log("HOST_FS_STAGE unmount-and-reuse\n")?;
    check_exit_cleanup(system, writable.id)?;
    require(files.volume(writable.id) == Err(Error::NotFound))?;
    require(files.catalog(writable.id, root.id) == Err(Error::NotFound))?;
    let replacement = files.mount_host_export(0)?;
    require(files.lookup(replacement.id, mac_toolbox::files::ROOT_ID, "Exit staging")
            == Err(Error::NotFound))?;
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

fn check_resources(system: &System<'_>, volume: VolumeId) -> Result<(), Error>
{
    use mac_toolbox::{ResourceKey, TypeCode, files::ROOT_ID};
    let files = system.files();
    let entry = files.lookup(volume, ROOT_ID, "ResEdit")?;
    let startup = files.volume_at(0)?.id;
    let original = files.lookup(startup, ROOT_ID, "/Applications/ResEdit")?;
    let expected = system.resources().open_file(startup, original.id)?;
    let mut actual = system.resources().open_file(volume, entry.id)?;
    require(files.unmount(volume) == Err(Error::ServiceBusy))?;
    let types = expected.type_count()?;
    require(actual.type_count()? == types)?;
    let mut large = false;
    for t in 0..types
    {
        let kind = expected.type_at(t)?;
        require(actual.type_at(t)? == kind)?;
        let count = expected.count(kind)?;
        require(actual.count(kind)? == count)?;
        for r in 0..count
        {
            let info = expected.at(kind, r)?;
            let got = actual.at(kind, r)?;
            require(got.info == info.info && got.has_name == info.has_name)?;
            require(actual.get(info.info.key)?.info == info.info)?;
            if info.has_name
            {
                require(actual.named(kind, info.info.name())?.info == info.info)?;
            }
            compare_resource(&actual, &expected, info.info.key, info.info.length)?;
            large |= info.info.length > 64 * 1024;
            let mut tail = [0xa5; 7];
            require(actual.read(info.info.key, info.info.length, &mut tail)? == 0
                    && tail == [0xa5; 7])?;
            require(actual.read(info.info.key, info.info.length + 1, &mut tail)
                    == Err(Error::BadArgument)
                    && tail == [0xa5; 7])?;
        }
        require(matches!(actual.at(kind, count), Err(Error::NotFound)))?;
    }
    require(large)?;
    require(actual.type_at(types) == Err(Error::NotFound))?;
    require(matches!(actual.get(ResourceKey { kind: TypeCode::new(*b"MISS"),
                                              id:   0 }),
                     Err(Error::NotFound)))?;
    for _ in 0..100
    {
        require(actual.type_count()? == types)?;
    }
    actual.close()?;
    require(actual.type_count() == Err(Error::NotFound))?;
    check_resource_mark(system, volume, entry.id, types)?;
    Ok(())
}

fn check_resource_mark(system: &System<'_>, volume: VolumeId, id: mac_toolbox::Cnid, types: u32)
                       -> Result<(), Error>
{
    use mac_toolbox::abi::{files::*, resources::FileResourceCall};
    system.log("HOST_FS_STAGE resource-position\n")?;
    let client = system.client();
    let mut file = FileCall { operation: OPEN,
                              volume: volume.0,
                              cnid: id.0,
                              fork: RESOURCE_FORK,
                              ..Default::default() };
    client.file(&mut file, "", &mut [])?;
    let stale = file.handle;
    file = FileCall { operation: SEEK,
                      handle: stale,
                      whence: SEEK_START,
                      offset: 17,
                      ..Default::default() };
    client.file(&mut file, "", &mut [])?;
    let mut resource = FileResourceCall { operation: mac_toolbox::abi::resources::COUNT_TYPES,
                                          handle: stale,
                                          ..Default::default() };
    client.file_resource(&mut resource, "", &mut [])?;
    require(resource.index == types)?;
    file = FileCall { operation: TELL,
                      handle: stale,
                      ..Default::default() };
    client.file(&mut file, "", &mut [])?;
    require(file.position == 17)?;
    system.log("HOST_FS_STAGE resource-reuse\n")?;
    file = FileCall { operation: CLOSE,
                      handle: stale,
                      ..Default::default() };
    client.file(&mut file, "", &mut [])?;
    file = FileCall { operation: OPEN,
                      volume: volume.0,
                      cnid: id.0,
                      fork: RESOURCE_FORK,
                      ..Default::default() };
    client.file(&mut file, "", &mut [])?;
    require(file.handle != stale && file.handle & 0xffff == stale & 0xffff)?;
    system.log("HOST_FS_STAGE resource-stale\n")?;
    require(client.file_resource(&mut resource, "", &mut []) == Err(Error::NotFound))?;
    resource.handle = file.handle ^ 0x1000_0000;
    require(client.file_resource(&mut resource, "", &mut []) == Err(Error::NotFound))?;
    resource.handle = file.handle;
    client.file_resource(&mut resource, "", &mut [])?;
    require(resource.index == types)?;
    let mut close = FileCall { operation: CLOSE,
                               handle: file.handle,
                               ..Default::default() };
    client.file(&mut close, "", &mut [])
}

#[inline(never)]
fn compare_resource(actual: &mac_toolbox::resources::ResourceFile<'_>,
                    expected: &mac_toolbox::resources::ResourceFile<'_>,
                    key: mac_toolbox::ResourceKey, length: usize)
                    -> Result<(), Error>
{
    // Cross the 64 KiB class-read boundary without an app-local allocator.
    // This isolated frame fits the native fixture's 256 KiB thread stack.
    let mut left = [0; 65537];
    let mut right = [0; 65537];
    let mut at = 0;
    while at < length
    {
        let count = (length - at).min(left.len());
        require(actual.read(key, at, &mut left[..count])? == count)?;
        require(expected.read(key, at, &mut right[..count])? == count)?;
        require(left[..count] == right[..count])?;
        at += count;
    }
    Ok(())
}

fn check_writes(system: &System<'_>, volume: VolumeId, forked: mac_toolbox::Cnid)
                -> Result<(), Error>
{
    use mac_toolbox::{Timestamp,
                      files::{DataFileSpec, Fork, ROOT_ID}};
    let files = system.files();
    let when = Timestamp::from_mac_microseconds(3_875_171_696_123_456);
    let spec = DataFileSpec { volume,
                              parent: ROOT_ID,
                              name: "Re\u{301}sume\u{301}.json",
                              replacing: None,
                              file_type: (*b"TEXT").into(),
                              creator: (*b"TEST").into() };
    let mut writer = files.create_data_at(spec, 131089, when)?;
    require(files.unmount(volume) == Err(Error::ServiceBusy))?;
    write_pattern(&mut writer, 131089, 17, 3)?;
    let entry = writer.finish()?;
    require(entry.name() == "Résumé.json"
            && entry.data_length == 131089
            && entry.resource_length == 0)?;
    require(entry.created_at() == when && entry.modified_at() == when)?;
    require(entry.file_type == spec.file_type && entry.creator == spec.creator)?;
    require(files.lookup(volume, ROOT_ID, "RE\u{301}SUME\u{301}.JSON")?.id == entry.id)?;
    check_pattern(system, volume, entry.id, Fork::Data, 131089, 17, 3)?;
    let mut duplicate = files.create_data_at(spec, 0, when)?;
    require(duplicate.finish() == Err(Error::AlreadyExists))?;
    let mut mismatched = files.create_data_at(DataFileSpec { replacing: Some(forked),
                                                             ..spec },
                                              0,
                                              when)?;
    require(mismatched.finish() == Err(Error::NotFound))?;
    let partial = DataFileSpec { name: "Unpublished",
                                 ..spec };
    let mut writer = files.create_data_at(partial, 4, when)?;
    writer.write_all(b"ab")?;
    require(writer.finish() == Err(Error::BadArgument))?;
    drop(writer);
    require(files.lookup(volume, ROOT_ID, "Unpublished") == Err(Error::NotFound))?;
    let mut zero = files.create_data_at(DataFileSpec { name: "Zero",
                                                       ..spec },
                                        0,
                                        when)?;
    let zero = zero.finish()?;
    require(zero.data_length == 0 && zero.resource_length == 0)?;
    let mut empty = files.open(volume, zero.id, Fork::Data)?;
    require(empty.read(&mut [0; 1])? == 0)?;
    drop(empty);
    // Replace only an exact confirmed ID; preserve original type/creator,
    // creation date and the complete original resource fork.
    let original = files.catalog(volume, forked)?;
    let replacement = DataFileSpec { parent: original.parent,
                                     name: original.name(),
                                     replacing: Some(forked),
                                     ..spec };
    let mut writer = files.create_data_at(replacement, 70021, when)?;
    write_pattern(&mut writer, 70021, 29, 5)?;
    let reader = files.open(volume, forked, Fork::Data)?;
    require(writer.finish() == Err(Error::ServiceBusy))?;
    drop(reader);
    let changed = writer.finish()?;
    require(changed.id == original.id
            && changed.name() == original.name()
            && changed.created_at() == original.created_at()
            && changed.modified_at() == when
            && changed.file_type == original.file_type
            && changed.creator == original.creator
            && changed.resource_length == original.resource_length
            && changed.finder_info == original.finder_info
            && changed.flags == original.flags)?;
    check_pattern(system, volume, forked, Fork::Data, 70021, 29, 5)?;
    check_pattern(system, volume, forked, Fork::Resource, 70003, 13, 11)?;
    let mut writer = files.create_data_at(replacement, 131089, when)?;
    write_pattern(&mut writer, 131089, 31, 7)?;
    writer.finish()?;
    check_pattern(system, volume, forked, Fork::Data, 131089, 31, 7)?;
    check_pattern(system, volume, forked, Fork::Resource, 70003, 13, 11)?;
    // The public escape hatch verifies generation/namespace behavior; normal
    // owned writers never expose their transaction token.
    use mac_toolbox::abi::files::*;
    let mut raw = DataWriteCall { operation: BEGIN_DATA_WRITE,
                                  volume: volume.0,
                                  parent: ROOT_ID.0,
                                  ..Default::default() };
    system.client().data_write(&mut raw, "Never published", &[])?;
    let stale = raw.handle;
    system.client().data_write(&mut DataWriteCall { operation: ABORT_DATA_WRITE,
                                                     handle: stale,
                                                     ..Default::default() },
                                "",
                                &[])?;
    raw = DataWriteCall { operation: BEGIN_DATA_WRITE,
                          volume: volume.0,
                          parent: ROOT_ID.0,
                          ..Default::default() };
    system.client().data_write(&mut raw, "Never published", &[])?;
    require(raw.handle != stale && raw.handle & 0xffff == stale & 0xffff)?;
    for operation in [APPEND_DATA_WRITE, COMMIT_DATA_WRITE, ABORT_DATA_WRITE]
    {
        require(system.client().data_write(&mut DataWriteCall { operation,
                                                                handle: stale,
                                                                ..Default::default() },
                                           "",
                                           &[])
                == Err(Error::NotFound))?;
    }
    require(system.client().data_write(&mut DataWriteCall { operation: ABORT_DATA_WRITE,
                                                            handle: raw.handle ^ 0x1000_0000,
                                                            ..Default::default() },
                                       "",
                                       &[])
            == Err(Error::NotFound))?;
    system.client().data_write(&mut DataWriteCall { operation: ABORT_DATA_WRITE,
                                                     handle: raw.handle,
                                                     ..Default::default() },
                                "",
                                &[])?;
    Ok(())
}
fn write_pattern(writer: &mut mac_toolbox::files::DataWriter<'_>, length: usize, factor: usize,
                 bias: usize)
                 -> Result<(), Error>
{
    let mut bytes = [0; 4096];
    let mut at = 0;
    while at < length
    {
        let count = (length - at).min(bytes.len());
        for (i, b) in bytes[..count].iter_mut().enumerate()
        {
            *b = ((at + i) * factor + bias) as u8;
        }
        writer.write_all(&bytes[..count])?;
        at += count;
    }
    Ok(())
}
fn check_pattern(system: &System<'_>, volume: VolumeId, node: mac_toolbox::Cnid,
                 kind: mac_toolbox::files::Fork, length: usize, factor: usize, bias: usize)
                 -> Result<(), Error>
{
    let mut fork = system.files().open(volume, node, kind)?;
    require(fork.len()? == length as u64)?;
    let mut bytes = [0; 4096];
    let mut at = 0;
    while at < length
    {
        let count = fork.read(&mut bytes)?;
        require(count != 0 && count <= length - at)?;
        for (i, b) in bytes[..count].iter().enumerate()
        {
            require(*b == ((at + i) * factor + bias) as u8)?;
        }
        at += count;
    }
    require(fork.read(&mut bytes)? == 0)?;
    fork.close()
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
