//! Real FSYS DRVR and resident FORK broker, exercised solely through Toolbox.
#![cfg(target_arch = "riscv64")]
#![forbid(unsafe_code)]
use mac_toolbox::{Cnid, Error, System, VolumeId,
                  files::{ArchiveError, CatalogInfo, DataFileSpec, Filesystem, Fork, ROOT_ID,
                          SeekFrom},
                  quickdraw::{Rect, icons::IconFlags}};
const NESTED: &[u8] = include_bytes!("../../mac-sit/tests/fixtures/nested-v1.sit");
const FORKS: &[u8] = include_bytes!("../../mac-sit/tests/fixtures/forks-v1.sit");
fn require(value: bool) -> Result<(), Error>
{
    if value { Ok(()) } else { Err(Error::BadAbi) }
}
fn explained(system: &System<'_>, actual: Error, cause: ArchiveError, expected: &str)
             -> Result<(), Error>
{
    use core::fmt::Write;
    require(actual == Error::Archive(cause))?;
    let mut text = super::Message { bytes:  [0; 128],
                                    length: 0 };
    write!(&mut text, "{actual}").map_err(|_| Error::BadAbi)?;
    let text = core::str::from_utf8(&text.bytes[..text.length]).map_err(|_| Error::BadAbi)?;
    require(text.contains(expected))?;
    system.log("STUFFIT_EXPECTED_FAILURE ")?;
    system.log(text)?;
    system.log("\n")
}
// Re-sign deliberately changed fixture headers so method rejection is tested
// after catalog validation, rather than being masked by a header CRC failure.
fn sign_entry(bytes: &mut [u8])
{
    let mut crc = 0u16;
    for byte in &bytes[22..132]
    {
        crc ^= u16::from(*byte);
        for _ in 0..8
        {
            crc = (crc >> 1) ^ if crc & 1 != 0 { 0xa001 } else { 0 };
        }
    }
    bytes[132..134].copy_from_slice(&crc.to_be_bytes());
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
fn icons(system: &System<'_>, present: bool) -> Result<(), Error>
{
    let world = system.quickdraw().new_gworld(32, 32)?;
    let icons = system.quickdraw().icons();
    let target = world.graphics().window_id();
    let flags = IconFlags::default();
    let mut pixels = [0; 1024];
    // File code wins over a misleading suffix; untyped host imports use the
    // driver's hint. Unknown and explicitly typed TEXT files keep their icons.
    for (kind, name) in
        [(*b"SIT!", "Archive.bin"), (*b"SITD", "Archive"), (*b"BINA", "Archive.SiT")]
    {
        for size in [16, 32]
        {
            world.set_pixels(&[0x123456; 1024])?;
            let result = icons.plot_file_default(target,
                                                 kind.into(),
                                                 name,
                                                 Rect::new(0, 0, size, size),
                                                 flags);
            if !present
            {
                require(result == Err(Error::NotFound))?;
                continue;
            }
            result?;
            require(world.pixels(&mut pixels)? == pixels.len())?;
            let mut black = 0;
            let mut white = 0;
            let mut colored = 0;
            for y in 0..32
            {
                for x in 0..32
                {
                    let pixel = pixels[y * 32 + x];
                    if x >= size as usize || y >= size as usize
                    {
                        require(pixel == 0x123456)?;
                    }
                    match pixel
                    {
                        0 => black += 1,
                        0xffffff => white += 1,
                        0x123456 => (),
                        _ => colored += 1
                    }
                }
            }
            require(black > 15 && white > 15 && colored > 4)?;
        }
    }
    for (kind, name) in [(*b"BINA", "Archive.sitx"), (*b"TEXT", "Archive.sit")]
    {
        require(icons.plot_file_default(target,
                                        kind.into(),
                                        name,
                                        Rect::new(0, 0, 32, 32),
                                        flags)
                == Err(Error::NotFound))?;
    }
    Ok(())
}
fn retained_updates(system: &System<'_>) -> Result<(), Error>
{
    use mac_toolbox::{Rect, events::EventMask, windows::WindowSpec};
    let mut window =
        system.windows()
              .create_owned(WindowSpec::new(Rect::new(50, 50, 250, 180), "Retained updates"))?;
    let id = window.id();
    system.windows().begin_update(id)?;
    require(system.windows().validate(id) == Err(Error::ServiceBusy))?;
    system.windows().end_update(id)?;
    system.windows().invalidate(id, Rect::new(4, 4, 20, 20))?;
    require(system.events().peek_positioned(EventMask::UPDATE)?.is_some())?;
    system.windows().validate(id)?;
    system.windows().validate(id)?;
    require(system.events().peek_positioned(EventMask::UPDATE)?.is_none())?;
    // Validation must not suppress subsequent damage or accept a reused ID.
    system.windows().invalidate(id, Rect::new(8, 8, 30, 30))?;
    require(system.events().peek_positioned(EventMask::UPDATE)?.is_some())?;
    window.close()?;
    let next =
        system.windows()
              .create_owned(WindowSpec::new(Rect::new(50, 50, 250, 180), "Replacement updates"))?;
    require(next.id() != id && (next.id().0 & 0xffff) == (id.0 & 0xffff))?;
    require(system.windows().validate(id) == Err(Error::NotFound))?;
    require(system.events().peek_positioned(EventMask::UPDATE)?.is_some())?;
    system.log("RETAINED_UPDATE_USERSPACE_PASS\n")
}
pub fn check(system: &System<'_>) -> Result<(), Error>
{
    retained_updates(system)?;
    let files = system.files();
    let startup = files.volume_at(0)?.id;
    if files.lookup(startup, ROOT_ID, "No StuffIt Driver").is_ok()
    {
        let archive = create(system, startup, "Examples.sit", NESTED)?;
        require(files.open_item(startup, archive.id)?.is_none())?;
        require(files.mount_file(startup, archive.id) == Err(Error::NotSupported))?;
        return icons(system, false);
    }
    system.log("STUFFIT_STAGE associations-and-icons\n")?;
    icons(system, true)?;
    system.log("STUFFIT_STAGE create\n")?;
    let archive = create(system, startup, "Examples.bin", NESTED)?;
    let forked = create(system, startup, "Forked.sit", FORKS)?;
    let duplicate = create(system, startup, "Copy.sit", NESTED)?;
    let mut public = files.open(startup, archive.id, Fork::Data)?;
    public.seek(SeekFrom::Start(17))?;
    system.log("STUFFIT_STAGE mount\n")?;
    let unknown = files.create_data(DataFileSpec { volume:    startup,
                                                   parent:    ROOT_ID,
                                                   name:      "Unclaimed.bin",
                                                   replacing: None,
                                                   file_type: (*b"BINA").into(),
                                                   creator:   (*b"TEST").into() },
                                    0)?
                       .finish()?;
    require(files.open_item(startup, unknown.id)?.is_none())?;
    let mount = files.open_item(startup, archive.id)?.ok_or(Error::BadAbi)?;
    let volume = mount.id;
    require(mount.read_only && mount.name() == "Examples.bin" && mount.capacity.is_none())?;
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
    require(files.move_into(volume, pattern.id, ROOT_ID) == Err(Error::ReadOnly))?;
    require(files.set_icon_location(volume,
                                    pattern.id,
                                    Some(mac_toolbox::Point { h: 12,
                                                              v: 34 }))
            == Err(Error::ReadOnly))?;
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
    explained(system,
              badfile.read(&mut bytes).err().ok_or(Error::BadAbi)?,
              ArchiveError::ForkChecksum,
              "extracted bytes failed verification")?;
    badfile.close()?;
    files.unmount(badvol)?;
    let bad = create(system, startup, "Not classic.sit", b"not an archive")?;
    explained(system,
              files.mount_file(startup, bad.id).err().ok_or(Error::BadAbi)?,
              ArchiveError::Truncated,
              "required header or fork bytes are missing")?;
    // Failed mounts release source leases and pending driver work: replacement
    // and the next mount must work immediately, without changing windows/apps.
    files.rename(startup, bad.id, "Rejected.sit")?;
    let mut unsupported = [0; NESTED.len()];
    unsupported.copy_from_slice(NESTED);
    unsupported[14] = 99;
    let bad = create(system, startup, "Future.sit", &unsupported)?;
    explained(system,
              files.mount_file(startup, bad.id).err().ok_or(Error::BadAbi)?,
              ArchiveError::Version(99),
              "archive version 99 is not supported")?;
    files.rename(startup, bad.id, "Future released.sit")?;
    unsupported[14] = 1;
    unsupported[132] ^= 1;
    let bad = create(system, startup, "Header CRC.sit", &unsupported)?;
    explained(system,
              files.mount_file(startup, bad.id).err().ok_or(Error::BadAbi)?,
              ArchiveError::HeaderChecksum,
              "catalog is damaged")?;
    files.rename(startup, bad.id, "Header released.sit")?;
    let mut method = [0; FORKS.len()];
    method.copy_from_slice(FORKS);
    for (byte, cause, message) in
        [(64, ArchiveError::Method(64), "compression method 64 is not supported"),
         (0x10, ArchiveError::Encrypted, "password-protected extraction is not supported")]
    {
        method[22] = byte;
        sign_entry(&mut method);
        let bad = create(system,
                         startup,
                         if byte == 64 { "Unknown method.sit" } else { "Encrypted.sit" },
                         &method)?;
        let mounted = files.mount_file(startup, bad.id)?;
        let entry = files.child(mounted.id, ROOT_ID, 0)?;
        explained(system,
                  files.open(mounted.id, entry.id, Fork::Resource).err().ok_or(Error::BadAbi)?,
                  cause,
                  message)?;
        files.unmount(mounted.id)?;
        files.rename(startup,
                     bad.id,
                     if byte == 64 { "Method released.sit" } else { "Encrypted released.sit" })?;
    }
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
    system.log("STUFFIT_STAGE transfer\n")?;
    let mut hosts = 0;
    let mut host_ids = [VolumeId::NONE; 2];
    // Both writable and read-only exports are mounted automatically at boot.
    let count = files.volume_count()?;
    for i in 0..count
    {
        let host = files.volume_at(i)?;
        if host.filesystem
           != (Filesystem::Driver { kind:    (*b"HOST").into(),
                                    version: 1 })
        {
            continue;
        }
        require(hosts < host_ids.len())?;
        host_ids[hosts] = host.id;
        hosts += 1;
    }
    require(hosts == 2)?;
    for id in host_ids
    {
        let host = files.volume(id)?;
        system.log("STUFFIT_STAGE transfer-mount\n")?;
        let archive = files.lookup(host.id, ROOT_ID, "Examples.sit")?;
        system.log("STUFFIT_STAGE transfer-open-source\n")?;
        let mut public = files.open(host.id, archive.id, Fork::Data)?;
        public.seek(SeekFrom::Start(17))?;
        system.log("STUFFIT_STAGE transfer-acquire\n")?;
        let mount = files.open_item(host.id, archive.id)?.ok_or(Error::BadAbi)?;
        system.log("STUFFIT_STAGE transfer-acquired\n")?;
        require(mount.read_only && mount.name() == "Examples.sit")?;
        system.log("STUFFIT_STAGE transfer-repeat\n")?;
        require(files.mount_file(host.id, archive.id)?.id == mount.id)?;
        system.log("STUFFIT_STAGE transfer-position\n")?;
        require(public.position()? == 17)?;
        public.close()?;
        system.log("STUFFIT_STAGE transfer-pin\n")?;
        require(files.unmount(host.id) == Err(Error::ServiceBusy))?;
        if !host.read_only
        {
            let spec = DataFileSpec { volume:    host.id,
                                      parent:    ROOT_ID,
                                      name:      "Examples.sit",
                                      replacing: Some(archive.id),
                                      file_type: (*b"SIT!").into(),
                                      creator:   (*b"SIT!").into() };
            match files.create_data(spec, 0)
            {
                Err(Error::ServiceBusy) => (),
                Err(error) => return Err(error),
                Ok(mut staged) => require(staged.finish() == Err(Error::ServiceBusy))?
            }
        }
        let pattern = files.lookup(mount.id, ROOT_ID, "Top/Sub/Pattern")?;
        system.log("STUFFIT_STAGE transfer-read\n")?;
        let mut file = files.open(mount.id, pattern.id, Fork::Data)?;
        require(file.seek(SeekFrom::Start(8187))? == 8187)?;
        require(file.read(&mut bytes)? == 5 && bytes[..5] == [251, 252, 253, 254, 255])?;
        file.close()?;
        files.unmount(mount.id)?;
        let again = files.mount_file(host.id, archive.id)?.id;
        require(again != mount.id)?;
        require(files.child(mount.id, ROOT_ID, 0) == Err(Error::NotFound))?;
        files.unmount(again)?;
        system.log("STUFFIT_STAGE transfer-close\n")?;
        if !host.read_only
        {
            // Unmount must wait for backing Close ACK, not merely queue it.
            let spec = DataFileSpec { volume:    host.id,
                                      parent:    ROOT_ID,
                                      name:      "Examples.sit",
                                      replacing: Some(archive.id),
                                      file_type: (*b"SIT!").into(),
                                      creator:   (*b"SIT!").into() };
            let mut replacement = files.create_data(spec, 0)?;
            replacement.finish()?;
        }
        files.unmount(host.id)?;
    }
    Ok(())
}
