//! Native guest assertions included by the host-filesystem application fixture.
#![cfg(target_arch = "riscv64")]
//! Canonical native mount assertions, compiled into the private userspace app.
use mac_toolbox::{Error, System, VolumeId, files::Filesystem};
#[path = "transfer_cases.rs"]
mod transfer_cases;
pub fn check_disconnect(system: &System<'_>, source: bool) -> Result<(), Error>
{
    transfer_cases::check_disconnect(system, source)
}
fn require(value: bool) -> Result<(), Error>
{
    if value { Ok(()) } else { Err(Error::BadAbi) }
}
fn require_named(system: &System<'_>, name: &str, value: bool) -> Result<(), Error>
{
    if !value
    {
        system.log("HOST_FS_ASSERT_FAILED ")?;
        system.log(name)?;
        system.log("\n")?;
    }
    require(value)
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
    let entry = files.lookup(volume, mac_toolbox::files::ROOT_ID, "Res Edit")?;
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
pub fn check(system: &System<'_>, gate: mac_toolbox::abi::TrapGate) -> Result<(), Error>
{
    check_native_writes(system)?;
    let files = system.files();
    // System, not this fixture or Finder, mounts device exports before starting
    // userspace. Repeated explicit requests must find those same live volumes.
    system.log("HOST_FS_STAGE automatic-mounts\n")?;
    require_named(system, "automatic-volume-count", files.volume_count()? == 3)?;
    let automatic_rw = files.volume_at(1)?;
    let automatic_ro = files.volume_at(2)?;
    let trash = files.trash_directory(automatic_rw.id, false);
    if let Err(error) = trash
    {
        crate::diagnostics::error(system, "HOST_FS_AUTOMATIC_RW_TRASH", error);
    }
    require_named(system, "automatic-rw-trash-absent", trash == Err(Error::NotFound))?;
    require_named(system,
                  "automatic-ro-trash-absent",
                  files.trash_directory(automatic_ro.id, false) == Err(Error::NotFound))?;
    require_named(system,
                  "automatic-rw-name-and-mode",
                  automatic_rw.name() == "Native Host" && !automatic_rw.read_only)?;
    require_named(system,
                  "automatic-ro-name-and-mode",
                  automatic_ro.name() == "Read Only Host" && automatic_ro.read_only)?;
    require_named(system,
                  "automatic-rw-mount-reuse",
                  files.mount_host_export(0)?.id == automatic_rw.id)?;
    require_named(system,
                  "automatic-ro-mount-reuse",
                  files.mount_host_export(1)?.id == automatic_ro.id)?;
    require_named(system, "automatic-mount-count-unchanged", files.volume_count()? == 3)?;
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
    require(root.is_directory() && root.name() == "Native Host" && root.child_count == Some(6))?;
    require(files.parent(writable.id, root.id)? == root)?;
    let mut folder = None;
    let mut file = None;
    for index in 0..6
    {
        let entry = files.child(writable.id, root.id, index)?;
        require(entry.parent == root.id)?;
        if entry.name() == "Tools"
        {
            folder = Some(entry);
        }
        if entry.name() == "Café"
        {
            file = Some(entry);
        }
    }
    let folder = folder.ok_or(Error::BadAbi)?;
    let file = file.ok_or(Error::BadAbi)?;
    require(folder.is_directory() && folder.name() == "Tools" && folder.child_count.is_none())?;
    require(!file.is_directory()
            && file.name() == "Café"
            && file.data_length == 23
            && file.resource_length == 0)?;
    require(files.catalog(writable.id, file.id)? == file)?;
    system.log("HOST_FS_STAGE comments\n")?;
    expect_comment(system, writable.id, root.id, "")?;
    expect_comment(system, writable.id, folder.id, "")?;
    expect_comment(system, writable.id, file.id, "Native comment: café 🦀")?;
    require(file.created.mac_microseconds() == Some(3_875_171_696_123_456)
            && file.modified.mac_microseconds() == Some(3_875_171_696_654_321))?;
    let mut tiny = [0xa5; 2];
    let mut comment = mac_toolbox::abi::files::FileCall { operation:
                                                              mac_toolbox::abi::files::COMMENT,
                                                          volume: writable.id.0,
                                                          cnid: file.id.0,
                                                          ..Default::default() };
    require(system.client().file(&mut comment, "", &mut tiny) == Err(Error::NoMemory)
            && tiny == [0xa5; 2]
            && comment.transferred == 0
            && comment.length == "Native comment: café 🦀".len() as u64)?;
    require(files.parent(writable.id, file.id)? == root)?;
    require(files.child(writable.id, root.id, 6) == Err(Error::NotFound))?;
    require(files.child(writable.id, file.id, 0) == Err(Error::BadArgument))?;
    let exact_folder = files.catalog(writable.id, folder.id)?;
    require(files.allocation_info(writable.id, file.id)?.total_bytes()?.is_none())?;
    require(files.allocation_info(writable.id, folder.id)?.total_bytes()? == Some(0))?;
    require(exact_folder.child_count == Some(4))?;
    let nested = files.lookup(writable.id, folder.id, "Child")?;
    let forks = files.lookup(writable.id, folder.id, "Forks")?;
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
    require(files.catalog(read_only.id, mac_toolbox::files::ROOT_ID)?.child_count == Some(5))?;
    require(files.child(read_only.id, mac_toolbox::files::ROOT_ID, 5) == Err(Error::NotFound))?;
    system.log("HOST_FS_STAGE resource-inspection\n")?;
    check_resources(system, read_only.id)?;
    let read_only_item = files.child(read_only.id, mac_toolbox::files::ROOT_ID, 0)?;
    expect_comment(system, read_only.id, mac_toolbox::files::ROOT_ID, "")?;
    expect_comment(system, read_only.id, read_only_item.id, "")?;
    require(files.move_into(read_only.id, read_only_item.id, mac_toolbox::files::ROOT_ID)
            == Err(Error::ReadOnly))?;
    require(files.new_folder(read_only.id, mac_toolbox::files::ROOT_ID, "Forbidden")
            == Err(Error::ReadOnly))?;
    require(files.set_icon_location(read_only.id,
                                    mac_toolbox::files::ROOT_ID,
                                    Some(mac_toolbox::Point { h: 12,
                                                              v: 34 }))
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
    system.log("HOST_FS_STAGE plain-host-entries-and-folders\n")?;
    check_plain(system, writable.id, true, gate)?;
    check_plain(system, read_only.id, false, gate)?;
    require(files.scan_directory(writable.id, mac_toolbox::files::ROOT_ID, &mut prior_scan)
            == Err(Error::ScanStale))?;
    system.log("HOST_FS_STAGE native-properties\n")?;
    check_object_properties(system, files.volume_at(0)?.id)?;
    system.log("HOST_FS_STAGE provider-properties\n")?;
    check_object_properties(system, writable.id)?;
    check_removal(system, writable.id, read_only.id, gate)?;
    check_trash_directory(system, writable.id, read_only.id, gate)?;
    transfer_cases::check(system, files.volume_at(0)?.id, writable.id, read_only.id, gate)?;
    system.log("HOST_FS_STAGE unmount-and-reuse\n")?;
    check_external_application(system, read_only.id, "Host Exit Worker", false, gate)?;
    check_exit_cleanup(system, writable.id, gate)?;
    require(files.volume(writable.id) == Err(Error::NotFound))?;
    require(files.catalog(writable.id, root.id) == Err(Error::NotFound))?;
    require(expect_comment(system, writable.id, file.id, "") == Err(Error::NotFound))?;
    let replacement = files.mount_host_export(0)?;
    require(files.lookup(replacement.id, mac_toolbox::files::ROOT_ID, "Exit staging")
            == Err(Error::NotFound))?;
    require(files.lookup(replacement.id, mac_toolbox::files::ROOT_ID, "Exit fork staging")
            == Err(Error::NotFound))?;
    require(files.scan_directory(replacement.id, mac_toolbox::files::ROOT_ID, &mut prior_scan)
            == Err(Error::BadArgument))?;
    require(replacement.id != writable.id && replacement.id.0 & 0xffff == writable.id.0 & 0xffff)?;
    require(expect_comment(system, writable.id, file.id, "") == Err(Error::NotFound))?;
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
    let entry = files.lookup(volume, ROOT_ID, "Res Edit")?;
    let startup = files.volume_at(0)?.id;
    let original = files.lookup(startup, ROOT_ID, "/Applications/Res Edit")?;
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
    system.log("HOST_FS_WRITE create\n")?;
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
    system.log("HOST_FS_WRITE duplicate\n")?;
    let mut duplicate = files.create_data_at(spec, 0, when)?;
    require(duplicate.finish() == Err(Error::AlreadyExists))?;
    system.log("HOST_FS_WRITE mismatch\n")?;
    let mut mismatched = files.create_data_at(DataFileSpec { replacing: Some(forked),
                                                             ..spec },
                                              0,
                                              when)?;
    require(mismatched.finish() == Err(Error::NotFound))?;
    let partial = DataFileSpec { name: "Unpublished",
                                 ..spec };
    system.log("HOST_FS_WRITE partial\n")?;
    let mut writer = files.create_data_at(partial, 4, when)?;
    writer.write_all(b"ab")?;
    require(writer.finish() == Err(Error::BadArgument))?;
    drop(writer);
    require(files.lookup(volume, ROOT_ID, "Unpublished") == Err(Error::NotFound))?;
    system.log("HOST_FS_WRITE zero\n")?;
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
    system.log("HOST_FS_WRITE replace\n")?;
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
    require(raw.handle != stale
            && raw.handle > u64::from(u32::MAX)
            && stale > u64::from(u32::MAX))?;
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
                    require(["Tools", "Host Folder"].contains(&catalog.name())
                            && data_allocated == Some(0)
                            && resource_allocated == Some(0))?;
                }
                else
                {
                    require(["Café", "Host File.txt", "Raw.sit", "Snapshot.txt"].contains(&catalog.name())
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
                require(count == 6)?;
                require(matches!(system.files().scan_directory(volume, ROOT_ID, &mut cursor)?,
                                 ScanStep::Done))?;
                return Ok(cursor);
            }
        }
    }
    Err(Error::ServiceBusy)
}

fn expect_comment(system: &System<'_>, volume: VolumeId, node: mac_toolbox::Cnid, expected: &str)
                  -> Result<(), Error>
{
    struct Match<'a>
    {
        remaining: &'a str
    }
    impl core::fmt::Write for Match<'_>
    {
        fn write_str(&mut self, text: &str) -> core::fmt::Result
        {
            self.remaining = self.remaining.strip_prefix(text).ok_or(core::fmt::Error)?;
            Ok(())
        }
    }
    let mut text = Match { remaining: expected };
    system.files().comment(volume, node, &mut text)?;
    require(text.remaining.is_empty())
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

fn check_external_application(system: &System<'_>, volume: VolumeId, path: &str, writable: bool,
                              gate: mac_toolbox::abi::TrapGate)
                              -> Result<(), Error>
{
    let files = system.files();
    let processes = system.processes();
    let own = processes.current()?;
    if writable
    {
        let broken = files.lookup(volume, mac_toolbox::files::ROOT_ID, "Tools/Broken")?;
        let error = processes.launch(volume, broken.id).err().ok_or(Error::BadAbi)?;
        require(error == Error::CorruptData)?;
        crate::diagnostics::error(system, "HOST_FS_EXTERNAL_APPLICATION_REJECTED", error);
        require(!processes.is_running(volume, broken.id)? && processes.count()? == 1)?;
    }
    let worker = files.lookup(volume, mac_toolbox::files::ROOT_ID, path)?;
    if !writable
    {
        let mut call =
            mac_toolbox::abi::processes::ProcessCall { operation:
                                                           mac_toolbox::abi::processes::LAUNCH,
                                                       volume: volume.0,
                                                       cnid: worker.id.0,
                                                       ..Default::default() };
        // SAFETY: live exclusive request; this conversion call contains no app
        // pointers and reaches the same owned resident loader as the native API.
        let status = unsafe {
            gate(mac_toolbox::abi::traps::PSTRING_PROCESS,
                 core::ptr::from_mut(&mut call).cast(),
                 core::mem::size_of_val(&call))
        };
        if status != 0
        {
            return Err(Error::from_status(status as i32));
        }
        require(call.info.name[0] == 16 && &call.info.name[1..17] == b"Host Exit Worker")?;
        system.log("HOST_FS_EXTERNAL_APPLICATION_PASCAL_PASS\n")?;
    }
    let launched = processes.launch(volume, worker.id)?;
    require(launched.cnid == worker.id
            && launched.volume == volume
            && launched.thread.0 >> 16 != 0)?;
    // Existing-instance activation must neither reread the fork nor spawn twice.
    let repeated = processes.launch(volume, worker.id)?;
    require(repeated.id == launched.id && processes.count()? == 2)?;
    system.log(if writable
               {
                   "HOST_FS_EXTERNAL_APPLICATION_RELEASE_RW\n"
               }
               else
               {
                   "HOST_FS_EXTERNAL_APPLICATION_RELEASE_RO\n"
               })?;
    for _ in 0..300
    {
        if !processes.is_running(volume, worker.id)?
        {
            break;
        }
        system.events().wait_event(mac_toolbox::EventMask::ALL, 1)?;
    }
    require(!processes.is_running(volume, worker.id)? && processes.count()? == 1)?;
    processes.activate(own.id)?;
    system.log("HOST_FS_EXTERNAL_APPLICATION_PASS\n")?;
    Ok(())
}

fn check_exit_cleanup(system: &System<'_>, volume: VolumeId, gate: mac_toolbox::abi::TrapGate)
                      -> Result<(), Error>
{
    let files = system.files();
    check_external_application(system, volume, "Tools/Host Exit Worker", true, gate)?;
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

fn check_metadata(system: &System<'_>, volume: VolumeId,
                  original: mac_toolbox::files::CatalogInfo,
                  folder: mac_toolbox::files::CatalogInfo, gate: mac_toolbox::abi::TrapGate)
                  -> Result<(), Error>
{
    use mac_toolbox::{Point, Timestamp,
                      files::{Fork, ROOT_ID}};
    let files = system.files();
    let when = Timestamp::from_mac_microseconds(3_875_171_696_987_654);
    files.set_comment_at(volume, original.id, "Native metadata: café 🦀", when)?;
    expect_comment(system, volume, original.id, "Native metadata: café 🦀")?;
    require(files.volume(volume)?.modified == when)?;
    let mut reader = files.open(volume, original.id, Fork::Data)?;
    let point = Point { h: 123,
                        v: 45 };
    require(files.set_icon_location_at(volume, original.id, Some(point), when)?
                 .saved_icon_location()
            == Some(point))?;
    require(files.set_comment_at(volume, original.id, "blocked", when)
            == Err(Error::AccessDenied))?;
    let mut data = [0; 16];
    require(reader.read(&mut data)? == 10 && &data[..10] == b"host data\n")?;
    reader.close()?;
    let locked = files.set_locked_at(volume, original.id, true, when)?;
    require(locked.is_locked())?;
    // Finder placement remains available even when the file's data is locked.
    require(files.set_icon_location_at(volume,
                                       original.id,
                                       Some(Point { h: 124,
                                                    v: 45 }),
                                       when)?
                 .is_locked())?;
    let error =
        files.set_comment_at(volume, original.id, "blocked", when).err().ok_or(Error::BadAbi)?;
    require(error == Error::AccessDenied)?;
    crate::diagnostics::error(system, "HOST_FS_METADATA_PROTECTED", error);
    require(!files.set_locked_at(volume, original.id, false, when)?.is_locked())?;
    require(files.set_stationery_at(volume, original.id, true, when)?.is_stationery())?;
    require(!files.set_stationery_at(volume, original.id, false, when)?.is_stationery())?;
    let current = files.catalog(volume, original.id)?;
    require(current.created == original.created
            && current.modified == original.modified
            && current.file_type == original.file_type
            && current.creator == original.creator
            && current.data_length == original.data_length
            && current.resource_length == 0)?;
    files.set_comment_at(volume, folder.id, "Raw folder metadata", when)?;
    expect_comment(system, volume, folder.id, "Raw folder metadata")?;
    require(files.set_locked_at(volume, folder.id, true, when) == Err(Error::BadArgument))?;
    let native = files.lookup(volume, ROOT_ID, "Tools")?;
    files.set_comment_at(volume, native.id, "Native folder metadata", when)?;
    expect_comment(system, volume, native.id, "Native folder metadata")?;
    let forked = files.lookup(volume, native.id, "Forks")?;
    files.set_comment_at(volume, forked.id, "Both forks retained", when)?;
    expect_comment(system, volume, forked.id, "Both forks retained")?;
    require(files.catalog(volume, forked.id)?.resource_length == forked.resource_length)?;
    // The original Pascal trap reaches the same transaction, preserving native
    // UTF-8 storage and converting MacRoman only at this boundary.
    let comment = b"\x0bLegacy Caf\x8e";
    let mut call =
        mac_toolbox::abi::files::FileCall { operation: mac_toolbox::abi::files::SET_COMMENT,
                                            volume: volume.0,
                                            cnid: original.id.0,
                                            position: when.mac_microseconds().unwrap(),
                                            name: comment.as_ptr(),
                                            name_length: comment.len(),
                                            ..Default::default() };
    // SAFETY: live exclusive request and bounded readable Pascal comment.
    let status = unsafe {
        gate(mac_toolbox::abi::traps::PSTRING_FILE,
             core::ptr::from_mut(&mut call).cast(),
             core::mem::size_of_val(&call))
    };
    if status != 0
    {
        return Err(Error::from_status(status as i32));
    }
    expect_comment(system, volume, original.id, "Legacy Café")?;
    metadata_snapshot(system, &current, &folder, &forked)?;
    system.log("HOST_FS_METADATA_PASS native=passed legacy=passed protected=passed \
                retry=passed\n")?;
    Ok(())
}

fn metadata_snapshot(system: &System<'_>, file: &mac_toolbox::files::CatalogInfo,
                     folder: &mac_toolbox::files::CatalogInfo,
                     native: &mac_toolbox::files::CatalogInfo)
                     -> Result<(), Error>
{
    use core::fmt::Write;
    struct Text
    {
        bytes:  [u8; 256],
        length: usize
    }
    impl core::fmt::Write for Text
    {
        fn write_str(&mut self, text: &str) -> core::fmt::Result
        {
            let end = self.length.checked_add(text.len()).ok_or(core::fmt::Error)?;
            self.bytes
                .get_mut(self.length..end)
                .ok_or(core::fmt::Error)?
                .copy_from_slice(text.as_bytes());
            self.length = end;
            Ok(())
        }
    }
    let mut text = Text { bytes:  [0; 256],
                          length: 0 };
    writeln!(&mut text,
             "HOST_FS_METADATA_SNAPSHOT file={} folder={} native={} created={} modified={} \
              native_created={} native_modified={}",
             file.id,
             folder.id,
             native.id,
             file.created.mac_microseconds().ok_or(Error::BadArgument)?,
             file.modified.mac_microseconds().ok_or(Error::BadArgument)?,
             native.created.mac_microseconds().ok_or(Error::BadArgument)?,
             native.modified.mac_microseconds().ok_or(Error::BadArgument)?).map_err(|_| {
                                                                               Error::NoMemory
                                                                           })?;
    system.log(core::str::from_utf8(&text.bytes[..text.length]).map_err(|_| Error::BadArgument)?)
}

pub fn check_metadata_restored(system: &System<'_>) -> Result<(), Error>
{
    use mac_toolbox::{Point,
                      files::{Fork, ROOT_ID}};
    let files = system.files();
    let volume = files.volume_at(1)?.id;
    let file = files.lookup(volume, ROOT_ID, "Host File.txt")?;
    let folder = files.lookup(volume, ROOT_ID, "Host Folder")?;
    let tools = files.lookup(volume, ROOT_ID, "Tools")?;
    let native = files.lookup(volume, tools.id, "Forks")?;
    expect_comment(system, volume, file.id, "Legacy Café")?;
    expect_comment(system, volume, folder.id, "Raw folder metadata")?;
    expect_comment(system, volume, tools.id, "Native folder metadata")?;
    expect_comment(system, volume, native.id, "Both forks retained")?;
    require(file.saved_icon_location()
            == Some(Point { h: 124,
                            v: 45 })
            && !file.is_locked()
            && !file.is_stationery())?;
    let mut data = files.open(volume, file.id, Fork::Data)?;
    let mut bytes = [0; 16];
    require(data.read(&mut bytes)? == 10 && &bytes[..10] == b"host data\n")?;
    data.close()?;
    require(files.catalog(mac_toolbox::VolumeId(volume.0 ^ 0x1000_0000), file.id)
            == Err(Error::NotFound))?;
    metadata_snapshot(system, &file, &folder, &native)
}

fn check_plain(system: &System<'_>, volume: VolumeId, writable: bool,
               gate: mac_toolbox::abi::TrapGate)
               -> Result<(), Error>
{
    use mac_toolbox::files::{DataFileSpec, Fork, ROOT_ID};
    let files = system.files();
    let file = files.lookup(volume, ROOT_ID, "Host File.txt")?;
    expect_comment(system, volume, file.id, "")?;
    system.log("HOST_FS_PLAIN raw-file\n")?;
    require(file.data_length == 10
            && file.resource_length == 0
            && file.modified.mac_microseconds().is_some_and(|t| t > 3_800_000_000_000_000))?;
    let mut fork = files.open(volume, file.id, Fork::Data)?;
    let mut bytes = [0; 128];
    require(fork.read(&mut bytes)? == 10 && &bytes[..10] == b"host data\n")?;
    fork.close()?;
    let mut fork = files.open(volume, file.id, Fork::Resource)?;
    require(fork.read(&mut bytes)? == 0)?;
    fork.close()?;
    let folder = files.lookup(volume, ROOT_ID, "Host Folder")?;
    expect_comment(system, volume, folder.id, "")?;
    system.log("HOST_FS_PLAIN raw-folder\n")?;
    require(folder.is_directory())?;
    if writable
    {
        let sit = files.lookup(volume, ROOT_ID, "Raw.sit")?;
        system.log("HOST_FS_PLAIN registry\n")?;
        require(sit.file_type == (*b"SIT!").into() && sit.creator == (*b"SIT!").into())?;
        let made = files.new_folder(volume, folder.id, "Guest Folder")?;
        system.log("HOST_FS_PLAIN created-folder\n")?;
        require(made.is_directory()
                && made.name() == "Guest Folder"
                && made.child_count == Some(0))?;
        require(files.new_folder(volume, folder.id, "guest folder") == Err(Error::AlreadyExists))?;
        let mut writer = files.create_data(DataFileSpec { volume,
                                                          parent: made.id,
                                                          name: "Child",
                                                          replacing: None,
                                                          file_type: (*b"TEXT").into(),
                                                          creator: (*b"TEST").into() },
                                           4)?;
        writer.write_all(b"data")?;
        let saved = writer.finish()?;
        require(files.lookup(volume, folder.id, "Guest Folder/Child")?.id == saved.id)?;
        let mut fork = files.open(volume, saved.id, Fork::Data)?;
        require(fork.read(&mut bytes)? == 4 && &bytes[..4] == b"data")?;
        fork.close()?;
    }
    else
    {
        require(files.new_folder(volume, folder.id, "Refused") == Err(Error::ReadOnly))?;
    }
    if writable
    {
        check_metadata(system, volume, file, folder, gate)?;
    }
    let snapshot = files.lookup(volume, ROOT_ID, "Snapshot.txt")?;
    system.log(if writable
               {
                   "HOST_FS_SNAPSHOT_READY_RW\n"
               }
               else
               {
                   "HOST_FS_SNAPSHOT_READY_RO\n"
               })?;
    let mut acknowledged = false;
    for _ in 0..240
    {
        let event = system.events().wait_event(mac_toolbox::EventMask::KEY_DOWN, 1)?;
        if matches!(event, mac_toolbox::Event::KeyDown { .. })
        {
            acknowledged = true;
            break;
        }
    }
    require(acknowledged)?;
    let error = files.open(volume, snapshot.id, Fork::Data).err().ok_or(Error::BadAbi)?;
    require(error == Error::ScanStale)?;
    crate::diagnostics::error(system, "HOST_FS_SNAPSHOT_STALE", error);
    let watch = files.watch_directory(volume, ROOT_ID)?;
    // Establish a resident background baseline before the host fixture changes.
    for _ in 0..40
    {
        system.events().wait_event(mac_toolbox::EventMask::ALL, 1)?;
    }
    // Drain the source-change notification and retry after normal catalog
    // invalidation. No window switch or provider remount is involved.
    let _ = watch.poll()?;
    let current = files.lookup(volume, ROOT_ID, "Snapshot.txt")?;
    require(current.id == snapshot.id)?;
    let mut data = files.open(volume, current.id, Fork::Data)?;
    require(data.read(&mut bytes)? == 6 && &bytes[..6] == b"after!")?;
    data.close()?;
    require(watch.poll()?.is_none())?;
    system.log("HOST_FS_SNAPSHOT_PASS\n")?;
    system.log(if writable { "HOST_FS_WATCH_READY_RW\n" } else { "HOST_FS_WATCH_READY_RO\n" })?;
    let mut changed = false;
    for _ in 0..240
    {
        system.events().wait_event(mac_toolbox::EventMask::ALL, 1)?;
        if watch.poll()?.is_some()
        {
            changed = true;
            break;
        }
    }
    require(changed)?;
    let arrived = files.lookup(volume, ROOT_ID, "Runtime Host File.txt")?;
    let mut data = files.open(volume, arrived.id, Fork::Data)?;
    require(data.read(&mut bytes)? == 4 && &bytes[..4] == b"live")?;
    data.close()?;
    watch.close()?;
    system.log("HOST_FS_WATCH_PASS\n")?;
    Ok(())
}


fn check_trash_directory(system: &System<'_>, writable: VolumeId, read_only: VolumeId,
                         gate: mac_toolbox::abi::TrapGate)
                         -> Result<(), Error>
{
    use mac_toolbox::abi::{files::{FileCall, TRASH_DIRECTORY},
                           traps::PSTRING_FILE};
    let files = system.files();
    let error = files.trash_directory(read_only, true).err().ok_or(Error::BadAbi)?;
    require(error == Error::ReadOnly)?;
    crate::diagnostics::error(system, "HOST_FS_TRASH_READ_ONLY", error);
    require(files.trash_directory(read_only, false) == Err(Error::NotFound))?;
    let trash = files.trash_directory(writable, true)?;
    require(trash.is_directory()
            && trash.name() == "Trash"
            && trash.parent == mac_toolbox::files::ROOT_ID
            && trash.child_count == Some(0))?;
    let date = files.volume(writable)?.modified;
    require(files.trash_directory(writable, true)?.id == trash.id)?;
    require(files.trash_directory(writable, false)?.id == trash.id)?;
    require(files.volume(writable)?.modified == date)?;
    let mut legacy = FileCall { operation: TRASH_DIRECTORY,
                                volume: writable.0,
                                index: 1,
                                ..Default::default() };
    let status = unsafe {
        gate(PSTRING_FILE, core::ptr::from_mut(&mut legacy).cast(), core::mem::size_of_val(&legacy))
    };
    require(status == 0
            && legacy.entry.id == trash.id.0
            && legacy.entry.name_length == 6
            && &legacy.entry.name[..6] == b"\x05Trash")?;
    require(files.volume(writable)?.modified == date)?;
    system.log("HOST_FS_TRASH_CREATE_PASS native=passed legacy=passed readonly=passed\n")
}

fn check_removal(system: &System<'_>, writable: VolumeId, readonly: VolumeId,
                 gate: mac_toolbox::abi::TrapGate)
                 -> Result<(), Error>
{
    use mac_toolbox::{abi::{files::*, traps::PSTRING_FILE},
                      files::{DataFileSpec, ROOT_ID}};
    let files = system.files();
    let folder = files.new_folder(writable, ROOT_ID, "Removal Acceptance")?;
    let mut writer = files.create_data(DataFileSpec { volume:    writable,
                                                      parent:    folder.id,
                                                      name:      "Document",
                                                      replacing: None,
                                                      file_type: (*b"TEXT").into(),
                                                      creator:   (*b"TEST").into() },
                                       0)?;
    let file = writer.finish()?;
    let error = files.remove(writable, folder.id).err().ok_or(Error::BadAbi)?;
    require(error == Error::DirectoryNotEmpty)?;
    crate::diagnostics::error(system, "HOST_FS_REMOVE_NONEMPTY", error);
    require(files.lookup(writable, folder.id, "Document")?.id == file.id)?;
    let ro_file = files.lookup(readonly, ROOT_ID, "Host File.txt")?;
    let error = files.remove(readonly, ro_file.id).err().ok_or(Error::BadAbi)?;
    require(error == Error::ReadOnly)?;
    crate::diagnostics::error(system, "HOST_FS_REMOVE_READ_ONLY", error);
    files.remove(writable, file.id)?;
    require(files.catalog(writable, file.id) == Err(Error::NotFound))?;
    require(files.lookup(writable, folder.id, "Document") == Err(Error::NotFound))?;
    let mut writer = files.create_data(DataFileSpec { volume:    writable,
                                                      parent:    folder.id,
                                                      name:      "Document",
                                                      replacing: None,
                                                      file_type: (*b"TEXT").into(),
                                                      creator:   (*b"TEST").into() },
                                       0)?;
    let replacement = writer.finish()?;
    require(replacement.id != file.id)?;
    require(files.catalog(writable, file.id) == Err(Error::NotFound))?;
    let mut call = FileCall { operation: REMOVE,
                              volume: writable.0,
                              cnid: replacement.id.0,
                              position: system.client()
                                              .now()?
                                              .mac_microseconds()
                                              .ok_or(Error::BadArgument)?,
                              ..Default::default() };
    let result = unsafe {
        gate(PSTRING_FILE, core::ptr::from_mut(&mut call).cast(), core::mem::size_of_val(&call))
    };
    require(result == 0)?;
    files.remove(writable, folder.id)?;
    let folder = files.new_folder(writable, ROOT_ID, "Removal Acceptance")?;
    files.remove(writable, folder.id)?;
    system.log("HOST_FS_REMOVE_PASS native=passed legacy=passed readonly=passed retry=passed\n")
}

fn check_object_properties(system: &System<'_>, volume: VolumeId) -> Result<(), Error>
{
    use mac_toolbox::{abi::dates::Timestamp,
                      files::{CatalogFlags, ForkMetadata, ROOT_ID}};
    let files = system.files();
    system.log("FILE_PROPERTIES_STAGE create ")?;
    system.log(files.volume(volume)?.name())?;
    system.log("\n")?;
    let folder = files.new_folder(volume, ROOT_ID, "Full Properties Acceptance")?;
    let mut metadata = ForkMetadata::from_catalog(&folder, "All properties café 🦀");
    metadata.created = Timestamp::from_mac_microseconds(3_875_171_696_123_456);
    metadata.modified = Timestamp::from_mac_microseconds(3_875_171_700_654_321);
    metadata.backup = Some(Timestamp::from_mac_microseconds(0));
    metadata.finder_info[..8].copy_from_slice(&[0, 40, 0, 30, 1, 40, 1, 90]);
    metadata.finder_info[14..20].copy_from_slice(&[0, 1, 0, 7, 0, 9]);
    system.log("FILE_PROPERTIES_STAGE set\n")?;
    let saved = files.set_properties(volume, folder.id, metadata)?;
    require(saved.id == folder.id
            && saved.parent == folder.parent
            && saved.name() == folder.name())?;
    require(saved.created == metadata.created
            && saved.modified == metadata.modified
            && saved.backup == metadata.backup
            && saved.finder_info == metadata.finder_info)?;
    expect_comment(system, volume, folder.id, metadata.comment)?;
    let date = files.volume(volume)?.modified;
    system.log("FILE_PROPERTIES_STAGE no-change\n")?;
    require(files.set_properties(volume, folder.id, metadata)? == saved)?;
    require(files.volume(volume)?.modified == date)?;
    let mut locked = metadata;
    locked.attributes = CatalogFlags::LOCKED;
    system.log("FILE_PROPERTIES_STAGE lock\n")?;
    files.set_properties(volume, folder.id, locked)?;
    let mut rejected = locked;
    rejected.comment = "Must not overwrite";
    let error = files.set_properties(volume, folder.id, rejected).err().ok_or(Error::BadAbi)?;
    require(error == Error::AccessDenied)?;
    crate::diagnostics::error(system, "FILE_PROPERTIES_LOCKED", error);
    expect_comment(system, volume, folder.id, metadata.comment)?;
    // Full-property unlock is valid only when every other property agrees.
    files.set_properties(volume, folder.id, metadata)?;
    let mut invalid = metadata;
    invalid.comment = "x"; // malformed backup presence is checked at raw boundary below.
    let mut call = mac_toolbox::abi::files::FileCall {
        operation: mac_toolbox::abi::files::SET_PROPERTIES,
        volume: volume.0, cnid: folder.id.0,
        entry: mac_toolbox::abi::files::CatalogInfo { backup_present: 0, backup: 1,
            ..Default::default() }, ..Default::default()
    };
    let error =
        system.client().file(&mut call, invalid.comment, &mut []).err().ok_or(Error::BadAbi)?;
    require(error == Error::BadArgument)?;
    crate::diagnostics::error(system, "FILE_PROPERTIES_INVALID", error);
    require(files.catalog(volume, folder.id)?.modified == metadata.modified)?;
    files.remove(volume, folder.id)?;
    system.log("FILE_PROPERTIES_PASS atomic=passed no-change=passed locked=passed retry=passed\n")
}

fn check_native_writes(system: &System<'_>) -> Result<(), Error>
{
    use mac_toolbox::{abi::files::*,
                      inspection::{CapacityPolicy, Scope}};
    let volume = system.files().volume_at(0)?.id;
    let mut ids = [0u64; 20];
    for (at, id) in ids.iter_mut().enumerate()
    {
        let name = [b'W', b'i', b'd', b'e', b' ', b'0' + (at / 10) as u8, b'0' + (at % 10) as u8];
        let mut call = DataWriteCall { operation: BEGIN_DATA_WRITE,
                                       volume: volume.0,
                                       parent: ROOT_ID,
                                       length: 0,
                                       ..Default::default() };
        system.client().data_write(&mut call,
                                    core::str::from_utf8(&name).map_err(|_| Error::BadAbi)?,
                                    &[])?;
        require(call.handle > u64::from(u32::MAX))?;
        *id = call.handle;
    }
    let pool = || -> Result<mac_toolbox::inspection::Pool, Error> {
        let ledger = system.inspection()
                           .snapshot(Scope::Own)
                           .map_err(|failure| failure.cause)?
                           .ledger(0)
                           .map_err(|failure| failure.cause)?;
        for at in 0..ledger.len()
        {
            let row = ledger.at(at).map_err(|failure| failure.cause)?;
            if row.kind == *b"FDWR"
            {
                return Ok(row);
            }
        }
        Err(Error::BadAbi)
    };
    let original = pool()?;
    require(original.live == 20 && original.capacity == CapacityPolicy::GrowableUntilMemory)?;
    let stale = ids[0];
    system.client().data_write(&mut DataWriteCall { operation: ABORT_DATA_WRITE,
                                                     handle: stale,
                                                     ..Default::default() },
                                "",
                                &[])?;
    let mut fresh = DataWriteCall { operation: BEGIN_DATA_WRITE,
                                    volume: volume.0,
                                    parent: ROOT_ID,
                                    length: 0,
                                    ..Default::default() };
    system.client().data_write(&mut fresh, "Wide replacement", &[])?;
    ids[0] = fresh.handle;
    require(fresh.handle != stale && pool()?.slots == original.slots)?;
    let error = system.client()
                      .data_write(&mut DataWriteCall { operation: ABORT_DATA_WRITE,
                                                       handle: stale,
                                                       ..Default::default() },
                                  "",
                                  &[])
                      .err()
                      .ok_or(Error::BadAbi)?;
    require(error == Error::NotFound)?;
    crate::diagnostics::error(system, "HOST_FS_NATIVE_STALE_WRITE", error);
    for handle in ids
    {
        system.client().data_write(&mut DataWriteCall { operation: ABORT_DATA_WRITE,
                                                         handle,
                                                         ..Default::default() },
                                    "",
                                    &[])?;
    }
    let remaining = pool()?;
    require(remaining.live == 0 && remaining.pending == 0)?;
    system.log("HOST_FS_NATIVE_WRITE_GROWTH_PASS count=20 width=64 reuse=passed cleanup=passed\n")?;
    Ok(())
}
