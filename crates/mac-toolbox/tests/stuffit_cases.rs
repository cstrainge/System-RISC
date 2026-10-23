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
    system.windows().set_standard_bounds(id, Some(Rect::new(80, 80, 300, 220)))?;
    require(system.window(id)?.bounds == Rect::new(50, 50, 250, 180)
            && !system.window(id)?.zoomed)?;
    require(system.windows().set_standard_bounds(id, Some(Rect::ZERO)) == Err(Error::BadArgument))?;
    require(system.windows().set_standard_bounds(id, Some(Rect::new(-100, 10, 300, 220)))
            == Err(Error::BadArgument))?;
    system.windows().set_standard_bounds(id, None)?;
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
    require(system.windows().set_standard_bounds(id, Some(Rect::new(80, 80, 300, 220)))
            == Err(Error::NotFound))?;
    require(system.events().peek_positioned(EventMask::UPDATE)?.is_some())?;
    system.log("RETAINED_UPDATE_USERSPACE_PASS\n")
}
fn check_v5(system: &System<'_>, source: VolumeId, archive: Cnid, bad: bool) -> Result<(), Error>
{
    use core::fmt::Write;
    let files = system.files();
    let mount = files.mount_file(source, archive)?;
    require(mount.read_only
            && mount.filesystem
               == Filesystem::Driver { kind:    (*b"SIT ").into(),
                                       version: 5 })?;
    let item = files.child(mount.id, ROOT_ID, 0)?;
    require(item.name() == "Arsenic"
            && item.data_length == 34384
            && item.resource_length == 1038
            && item.file_type == (*b"TEXT").into())?;
    let mut comment = super::Message { bytes:  [0; 128],
                                       length: 0 };
    files.comment(mount.id, item.id, &mut comment)?;
    require(&comment.bytes[..comment.length] == b"Project-owned bounded decoder fixture")?;
    require(files.rename(mount.id, item.id, "Refused") == Err(Error::ReadOnly))?;
    let mut buffer = [0; 4096];
    for (fork, length) in [(Fork::Resource, 1038), (Fork::Data, 34384)]
    {
        let mut file = files.open(mount.id, item.id, fork)?;
        let (mut at, mut state) = (0, 0x12345678u32);
        let mut rejected = false;
        loop
        {
            match file.read(&mut buffer)
            {
                Ok(0) => break,
                Ok(count) =>
                {
                    for byte in &buffer[..count]
                    {
                        let expected = if matches!(fork, Fork::Resource)
                        {
                            if at < 14 { b"Resource fork\r"[at] } else { (at - 14) as u8 }
                        }
                        else if at < 32768
                        {
                            state ^= state << 13;
                            state ^= state >> 17;
                            state ^= state << 5;
                            state as u8
                        }
                        else if at < 33868
                        {
                            0x90
                        }
                        else if at < 33872
                        {
                            b'A'
                        }
                        else
                        {
                            (at - 33872) as u8
                        };
                        require(*byte == expected)?;
                        at += 1;
                    }
                }
                Err(error) if bad && matches!(fork, Fork::Data) =>
                {
                    explained(system,
                              error,
                              ArchiveError::ForkChecksum,
                              "extracted bytes failed verification")?;
                    rejected = true;
                    break;
                }
                Err(error) => return Err(error)
            }
        }
        require(if bad && matches!(fork, Fork::Data)
                {
                    rejected
                }
                else
                {
                    at == length && !rejected
                })?;
        file.close()?;
    }
    files.unmount(mount.id)?;
    let mut message = super::Message { bytes:  [0; 128],
                                       length: 0 };
    let _ = writeln!(&mut message, "STUFFIT_V5_USERSPACE_PASS bad_crc={bad}");
    system.log(core::str::from_utf8(&message.bytes[..message.length]).map_err(|_| Error::BadAbi)?)
}
fn reference(system: &System<'_>, source: VolumeId, archive: Cnid) -> Result<(), Error>
{
    let files = system.files();
    let mount = files.mount_file(source, archive)?;
    require(mount.filesystem
            == Filesystem::Driver { kind:    (*b"SIT ").into(),
                                    version: 5 })?;
    let top = files.child(mount.id, ROOT_ID, 0)?;
    require(top.name() == "Aldus SuperPaint 3.5" && top.child_count == Some(5))?;
    let mut buffer = [0; 4096];
    let mut nonempty = 0;
    for index in 0..5
    {
        let entry = files.child(mount.id, top.id, index)?;
        require(!entry.is_directory())?;
        for (fork, expected) in
            [(Fork::Data, entry.data_length), (Fork::Resource, entry.resource_length)]
        {
            let mut file = files.open(mount.id, entry.id, fork)?;
            let mut total = 0u64;
            loop
            {
                let count = file.read(&mut buffer)?;
                if count == 0
                {
                    break;
                }
                total += count as u64;
            }
            require(total == expected)?;
            if total != 0
            {
                nonempty += 1;
            }
            file.close()?;
        }
    }
    require(nonempty == 9)?;
    files.unmount(mount.id)?;
    system.log("STUFFIT_REFERENCE_USERSPACE_PASS forks=9\n")
}
fn versions(system: &System<'_>, startup: VolumeId) -> Result<(), Error>
{
    let files = system.files();
    let mut later = [0; NESTED.len()];
    later.copy_from_slice(NESTED);
    later[14] = 2;
    later[16..20].copy_from_slice(&22u32.to_be_bytes());
    let archive = create(system, startup, "Later classic.sit", &later)?;
    let mount = files.mount_file(startup, archive.id)?;
    require(mount.filesystem
            == Filesystem::Driver { kind:    (*b"SIT ").into(),
                                    version: 2 })?;
    let pattern = files.lookup(mount.id, ROOT_ID, "Top/Sub/Pattern")?;
    let mut file = files.open(mount.id, pattern.id, Fork::Data)?;
    file.seek(SeekFrom::Start(8187))?;
    let mut tail = [0; 5];
    require(file.read(&mut tail)? == 5 && tail == [251, 252, 253, 254, 255])?;
    file.close()?;
    files.unmount(mount.id)?;
    for (name, bytes, bad) in
        [("Arsenic.sit",
          include_bytes!("../../mac-sit/tests/fixtures/arsenic-v5.sit").as_slice(),
          false),
         ("Randomized.sit",
          include_bytes!("../../mac-sit/tests/fixtures/arsenic-randomized-v5.sit").as_slice(),
          false),
         ("Bad Arsenic.sit",
          include_bytes!("../../mac-sit/tests/fixtures/arsenic-bad-crc-v5.sit").as_slice(),
          true)]
    {
        let archive = create(system, startup, name, bytes)?;
        check_v5(system, startup, archive.id, bad)?;
        files.rename(startup,
                     archive.id,
                     if bad
                     {
                         "Bad Arsenic released.sit"
                     }
                     else if name == "Arsenic.sit"
                     {
                         "Arsenic released.sit"
                     }
                     else
                     {
                         "Randomized released.sit"
                     })?;
    }
    system.log("STUFFIT_CLASSIC2_USERSPACE_PASS\n")
}
fn method13_byte(at: usize, resource: bool, window: bool) -> u8
{
    if resource
    {
        if at < 414
        {
            b"Resource fork\r"[at % 14]
        }
        else if at == 414
        {
            0
        }
        else
        {
            0x90
        }
    }
    else if window
    {
        at as u8
    }
    else
    {
        match at
        {
            0..=2 => 0,
            3..=2050 => (at - 3) as u8,
            2051 => 0x90,
            2052..=2151 => b'X',
            _ => b'!'
        }
    }
}
fn check_method13(system: &System<'_>, source: VolumeId, archive: Cnid, version: u32, bad: bool,
                  window: bool)
                  -> Result<(), Error>
{
    let files = system.files();
    let mount = files.mount_file(source, archive)?;
    require(mount.read_only
            && mount.filesystem
               == Filesystem::Driver { kind: (*b"SIT ").into(),
                                       version })?;
    let entry = files.child(mount.id, ROOT_ID, 0)?;
    let expected = if window { 99969 } else { 2153 };
    require(entry.name() == "Method13"
            && entry.data_length == expected
            && entry.resource_length == 416)?;
    let mut buffer = [0; 4096];
    let mut file = files.open(mount.id, entry.id, Fork::Data)?;
    if bad
    {
        for _ in 0..2
        {
            file.seek(SeekFrom::Start(0))?;
            explained(system,
                      file.read(&mut buffer).err().ok_or(Error::BadAbi)?,
                      ArchiveError::ForkChecksum,
                      "extracted bytes failed verification")?;
        }
    }
    else
    {
        let mut total = 0;
        loop
        {
            let limit = if window { buffer.len() } else { 257 };
            let count = file.read(&mut buffer[..limit])?;
            if count == 0
            {
                break;
            }
            for (i, byte) in buffer[..count].iter().enumerate()
            {
                require(*byte == method13_byte(total + i, false, window))?;
            }
            total += count;
        }
        require(total as u64 == expected)?;
        file.seek(SeekFrom::Start(7))?;
        require(file.read(&mut buffer[..23])? == 23)?;
        for (i, byte) in buffer[..23].iter().enumerate()
        {
            require(*byte == method13_byte(i + 7, false, window))?;
        }
    }
    file.close()?;
    // A failed data fork must leave the other fork readable immediately.
    let mut file = files.open(mount.id, entry.id, Fork::Resource)?;
    require(file.read(&mut buffer)? == 416)?;
    for (i, byte) in buffer[..416].iter().enumerate()
    {
        require(*byte == method13_byte(i, true, false))?;
    }
    file.close()?;
    files.unmount(mount.id)?;
    system.log(if bad
               {
                   "STUFFIT_METHOD13_USERSPACE_PASS bad_crc=true\n"
               }
               else
               {
                   "STUFFIT_METHOD13_USERSPACE_PASS bad_crc=false\n"
               })
}
fn method13(system: &System<'_>, volume: VolumeId) -> Result<(), Error>
{
    let variants: &[(&str, &[u8], u32)] =
        &[("M13 book1 v2.sit",
           include_bytes!("../../mac-sit/tests/fixtures/method13-book1-v2.sit"),
           2),
          ("M13 book2 v2.sit",
           include_bytes!("../../mac-sit/tests/fixtures/method13-book2-v2.sit"),
           2),
          ("M13 book3 v2.sit",
           include_bytes!("../../mac-sit/tests/fixtures/method13-book3-v2.sit"),
           2),
          ("M13 book4 v2.sit",
           include_bytes!("../../mac-sit/tests/fixtures/method13-book4-v2.sit"),
           2),
          ("M13 book5 v2.sit",
           include_bytes!("../../mac-sit/tests/fixtures/method13-book5-v2.sit"),
           2),
          ("M13 book1 v5.sit",
           include_bytes!("../../mac-sit/tests/fixtures/method13-book1-v5.sit"),
           5),
          ("M13 book2 v5.sit",
           include_bytes!("../../mac-sit/tests/fixtures/method13-book2-v5.sit"),
           5),
          ("M13 book3 v5.sit",
           include_bytes!("../../mac-sit/tests/fixtures/method13-book3-v5.sit"),
           5),
          ("M13 book4 v5.sit",
           include_bytes!("../../mac-sit/tests/fixtures/method13-book4-v5.sit"),
           5),
          ("M13 book5 v5.sit",
           include_bytes!("../../mac-sit/tests/fixtures/method13-book5-v5.sit"),
           5),
          ("M13 shared v2.sit",
           include_bytes!("../../mac-sit/tests/fixtures/method13-shared-v2.sit"),
           2),
          ("M13 shared v5.sit",
           include_bytes!("../../mac-sit/tests/fixtures/method13-shared-v5.sit"),
           5),
          ("M13 separate v2.sit",
           include_bytes!("../../mac-sit/tests/fixtures/method13-separate-v2.sit"),
           2),
          ("M13 separate v5.sit",
           include_bytes!("../../mac-sit/tests/fixtures/method13-separate-v5.sit"),
           5),
          ("M13 window.sit",
           include_bytes!("../../mac-sit/tests/fixtures/method13-window-v5.sit"),
           5),
          ("M13 independent.sit",
           include_bytes!("../../mac-sit/tests/fixtures/method13-independent-v5.sit"),
           5),
          ("M13 bad CRC.sit",
           include_bytes!("../../mac-sit/tests/fixtures/method13-bad-crc-v5.sit"),
           5)];
    for &(name, bytes, version) in variants
    {
        let file = create(system, volume, name, bytes)?;
        check_method13(system,
                       volume,
                       file.id,
                       version,
                       name == "M13 bad CRC.sit",
                       name == "M13 window.sit")?;
        // Acknowledged teardown must immediately release the source lease.
        system.files().rename(volume, file.id, "M13 released.sit")?;
        system.files().move_to_trash(volume, file.id)?;
        system.files().empty_trash(volume)?;
    }
    // Damage a payload codebook selector without changing catalog lengths or
    // CRCs. Header admission must succeed, with a useful read error afterwards.
    const VALID: &[u8] = include_bytes!("../../mac-sit/tests/fixtures/method13-book4-v2.sit");
    let mut broken = [0; VALID.len()];
    broken.copy_from_slice(VALID);
    let resource_packed =
        u32::from_be_bytes(VALID[114..118].try_into().map_err(|_| Error::BadAbi)?);
    broken[134 + resource_packed as usize] = 0xf0;
    let archive = create(system, volume, "M13 malformed.sit", &broken)?;
    let files = system.files();
    let mount = files.mount_file(volume, archive.id)?;
    let item = files.child(mount.id, ROOT_ID, 0)?;
    let mut file = files.open(mount.id, item.id, Fork::Data)?;
    explained(system,
              file.read(&mut [0; 4096]).err().ok_or(Error::BadAbi)?,
              ArchiveError::Malformed,
              "invalid lengths, hierarchy or compressed data")?;
    file.close()?;
    let mut file = files.open(mount.id, item.id, Fork::Resource)?;
    let mut bytes = [0; 416];
    require(file.read(&mut bytes)? == bytes.len() && bytes[414..] == [0, 0x90])?;
    file.close()?;
    files.unmount(mount.id)?;
    files.rename(volume, archive.id, "M13 malformed released.sit")?;
    system.log("STUFFIT_METHOD13_MALFORMED_USERSPACE_PASS\n")
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
    check_archive_in_archive(system, startup)?;
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
    system.log("STUFFIT_STAGE versions\n")?;
    versions(system, startup)?;
    system.log("STUFFIT_STAGE method13\n")?;
    method13(system, startup)?;
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
        if let Ok(archive) = files.lookup(host.id, ROOT_ID, "Reference.sit")
        {
            reference(system, host.id, archive.id)?;
        }
        let archive5 = files.lookup(host.id, ROOT_ID, "Arsenic.sit")?;
        check_v5(system, host.id, archive5.id, false)?;
        for (variant, version, bad) in
            [("shared-v2", 2, false), ("separate-v5", 5, false), ("bad-crc-v5", 5, true)]
        {
            let name = match variant
            {
                "shared-v2" => "Method13-shared-v2.sit",
                "separate-v5" => "Method13-separate-v5.sit",
                _ => "Method13-bad-crc-v5.sit"
            };
            let archive = files.lookup(host.id, ROOT_ID, name)?;
            check_method13(system, host.id, archive.id, version, bad, false)?;
        }
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
        check_recursive_transfer(system, mount.id, startup, host.id, host.read_only)?;
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

fn check_recursive_transfer(system: &System<'_>, archive: VolumeId, startup: VolumeId,
                            host: VolumeId, read_only: bool)
                            -> Result<(), Error>
{
    use mac_toolbox::{events::EventMask, files::*};
    let files = system.files();
    let source = files.lookup(archive, ROOT_ID, "Top")?;
    let target = files.new_folder(startup,
                                  ROOT_ID,
                                  if read_only { "Archive Copy RO" } else { "Archive Copy RW" })?;
    let spec = |volume, parent, mode| {
        TransferSpec { mode,
                       destination: TransferDestination::Folder { volume,
                                                                  parent } }
    };
    let input = TransferSource::new(archive, source.id, ROOT_ID);
    let run = |spec| -> core::result::Result<(), Error> {
        let mut transfer = files.begin_transfer(spec, &[input])?;
        for _ in 0..10000
        {
            let progress = transfer.step().map_err(|e| e.cause)?;
            if progress.finished()
            {
                return require(progress.phase == TransferPhase::Committed);
            }
            system.events().wait_event(EventMask::ALL, 1)?;
        }
        Err(Error::ServiceBusy)
    };
    let denied = run(spec(startup, target.id, TransferMode::Move));
    require(denied == Err(Error::ReadOnly))?;
    require(files.lookup(startup, target.id, "Top") == Err(Error::NotFound))?;
    run(spec(startup, target.id, TransferMode::Copy))?;
    compare_tree(system, &source, &files.lookup(startup, target.id, "Top")?, 0)?;
    if !read_only
    {
        let destination = files.new_folder(host, ROOT_ID, "Archive Copy")?;
        run(spec(host, destination.id, TransferMode::Copy))?;
        compare_tree(system, &source, &files.lookup(host, destination.id, "Top")?, 0)?;
    }
    system.log("STUFFIT_RECURSIVE_TRANSFER_PASS both-forks=passed metadata=passed readonly=passed \
                retry=passed\n")
}

#[inline(never)]
fn compare_tree(system: &System<'_>, source: &CatalogInfo, destination: &CatalogInfo, depth: usize)
                -> Result<(), Error>
{
    require(depth < 16
            && source.name() == destination.name()
            && source.kind == destination.kind
            && source.data_length == destination.data_length
            && source.resource_length == destination.resource_length
            && source.created == destination.created
            && source.modified == destination.modified
            && source.finder_info == destination.finder_info)?;
    let files = system.files();
    if source.is_directory()
    {
        let mut cursor = mac_toolbox::files::DirectoryCursor::default();
        loop
        {
            match files.scan_directory(source.volume, source.id, &mut cursor)?
            {
                mac_toolbox::files::ScanStep::Entry { catalog: child,
                                                      .. } =>
                {
                    let copied = files.lookup(destination.volume, destination.id, child.name())?;
                    compare_tree(system, &child, &copied, depth + 1)?;
                }
                mac_toolbox::files::ScanStep::Done => break,
                mac_toolbox::files::ScanStep::Pending =>
                {
                    system.events().wait_event(mac_toolbox::events::EventMask::ALL, 1)?;
                }
            }
        }
    }
    else
    {
        for fork in [Fork::Data, Fork::Resource]
        {
            let mut original = files.open(source.volume, source.id, fork)?;
            let mut copied = files.open(destination.volume, destination.id, fork)?;
            let mut a = [0; 4096];
            let mut b = [0; 4096];
            loop
            {
                let count = original.read(&mut a)?;
                let mut at = 0;
                while at < count
                {
                    let n = copied.read(&mut b[at..count])?;
                    require(n != 0)?;
                    at += n;
                }
                require(a[..count] == b[..count])?;
                if count == 0
                {
                    require(copied.read(&mut b)? == 0)?;
                    break;
                }
            }
            original.close()?;
            copied.close()?;
        }
    }
    Ok(())
}

fn check_archive_in_archive(system: &System<'_>, startup: VolumeId) -> Result<(), Error>
{
    const STORED: &[u8] =
        include_bytes!("../../mac-sit/tests/fixtures/archive-in-archive-stored.sit");
    const RLE: &[u8] = include_bytes!("../../mac-sit/tests/fixtures/archive-in-archive-rle.sit");
    let files = system.files();
    for (name, bytes) in [("Outer stored.sit", STORED), ("Outer RLE.sit", RLE)]
    {
        let source = create(system, startup, name, bytes)?;
        let outer = files.open_item(startup, source.id)?.ok_or(Error::BadAbi)?;
        let inner_source = files.lookup(outer.id, ROOT_ID, "Inner.sit")?;
        require(inner_source.file_type == (*b"SIT!").into())?;
        let inner = files.open_item(outer.id, inner_source.id)?.ok_or(Error::BadAbi)?;
        require(inner.read_only && inner.name() == "Inner.sit")?;
        require(files.mount_file(outer.id, inner_source.id)?.id == inner.id)?;
        require(files.unmount(outer.id) == Err(Error::ServiceBusy))?;
        let pattern = files.lookup(inner.id, ROOT_ID, "Top/Sub/Pattern")?;
        let mut file = files.open(inner.id, pattern.id, Fork::Data)?;
        let mut output = [0; 4096];
        for offset in [0, 4096, 7, 8000, 19]
        {
            file.seek(SeekFrom::Start(offset))?;
            let n = file.read(&mut output)?;
            require(n == (8192 - offset as usize).min(output.len()))?;
            for (i, byte) in output[..n].iter().enumerate()
            {
                require(*byte == (offset as usize + i) as u8)?;
            }
        }
        require(files.unmount(inner.id) == Err(Error::ServiceBusy))?;
        file.close()?;
        files.unmount(inner.id)?;
        require(files.catalog(inner.id, ROOT_ID) == Err(Error::NotFound))?;
        // Failure must not poison the still-mounted outer volume or its source.
        let mut source_file = files.open(outer.id, inner_source.id, Fork::Data)?;
        let mut header = [0; 22];
        require(source_file.read(&mut header)? == header.len())?;
        require(header.as_slice() == &NESTED[..22])?;
        source_file.close()?;
        files.unmount(outer.id)?;
        files.remove(startup, source.id)?;
    }
    system.log("STUFFIT_ARCHIVE_IN_ARCHIVE_USERSPACE_PASS wrappers=stored,rle backward=passed \
                teardown=passed\n")
}
