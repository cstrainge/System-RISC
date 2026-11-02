//! Real target application using the public SDK and actual FILE trap. These
//! finite observations do not qualify std, general large-gap I/O or pressure.
#![no_std]
#![no_main]
use mac_toolbox::{Error, System, ToolboxResult,
                  abi::{self, files::*}};

fn call(system: &System<'_>, operation: u32, handle: u32) -> ToolboxResult<FileCall>
{
    let mut call = FileCall { operation,
                              handle,
                              ..Default::default() };
    system.client().file(&mut call, "", &mut [])?;
    Ok(call)
}
fn seek(system: &System<'_>, handle: u32, offset: i64) -> ToolboxResult<()>
{
    let mut call = FileCall { operation: SEEK,
                              handle,
                              whence: SEEK_START,
                              offset,
                              ..Default::default() };
    system.client().file(&mut call, "", &mut [])
}
fn open(system: &System<'_>, volume: u32, cnid: u32, kind: u32, flags: u32, name: &str)
        -> ToolboxResult<FileCall>
{
    let mut call = FileCall { operation: OPEN_OPTIONS,
                              volume,
                              cnid,
                              fork: kind,
                              open_flags: flags,
                              ..Default::default() };
    system.client().file(&mut call, name, &mut [])?;
    Ok(call)
}
fn write(system: &System<'_>, handle: u32, bytes: &[u8]) -> ToolboxResult<()>
{
    let mut call = FileCall { handle,
                              ..Default::default() };
    system.client().file_write(&mut call, bytes)?;
    assert_eq!(call.transferred, bytes.len(), "bounded native write short prefix");
    Ok(())
}
fn length(system: &System<'_>, handle: u32, target: u64) -> ToolboxResult<()>
{
    let mut call = FileCall { operation: SET_LENGTH,
                              handle,
                              length: target,
                              ..Default::default() };
    system.client().file(&mut call, "", &mut [])?;
    assert_eq!(call.length, target, "small native resize length");
    Ok(())
}
fn read(system: &System<'_>, handle: u32, output: &mut [u8]) -> ToolboxResult<usize>
{
    let mut call = FileCall { operation: READ,
                              handle,
                              ..Default::default() };
    system.client().file(&mut call, "", output)?;
    Ok(call.transferred)
}
fn run(system: &System<'_>) -> ToolboxResult<()>
{
    system.log("STD_FILES_NATIVE_BEGIN\n")?;
    let mut volume = None;
    for index in 0..system.files().volume_count()?
    {
        let info = system.files().volume_at(index)?;
        let mut wire = FileCall { operation: VOLUME_INFO,
                                  volume: info.id.0,
                                  ..Default::default() };
        system.client().file(&mut wire, "", &mut [])?;
        if wire.volume_info.startup != 0
        {
            volume = Some(info.id.0);
            break;
        }
    }
    let volume = volume.ok_or(Error::NotFound)?;
    let mut lookup = FileCall { operation: LOOKUP,
                                volume,
                                cnid: ROOT_ID,
                                ..Default::default() };
    match system.client().file(&mut lookup, "Std Native File", &mut [])
    {
        Ok(()) =>
        {
            let opened = open(system, volume, lookup.entry.id, DATA_FORK, OPEN_READ, "")?;
            let mut bytes = [0u8; 4];
            assert_eq!(read(system, opened.handle, &mut bytes)?, 4);
            assert_eq!(&bytes, b"SYNC", "native persisted data after fresh guest boot");
            call(system, CLOSE, opened.handle)?;
            system.log("STD_FILES_NATIVE_REBOOT_BYTES_OBSERVED\n")?;
            return Ok(());
        }
        Err(Error::NotFound) => (),
        Err(error) => return Err(error)
    }
    system.log("STD_FILES_NATIVE_CREATE_BEGIN\n")?;
    let created = open(system,
                       volume,
                       ROOT_ID,
                       DATA_FORK,
                       OPEN_READ | OPEN_WRITE | OPEN_CREATE_NEW,
                       "Std Native File")?;
    assert_eq!(created.length, 0);
    let id = created.cnid;
    let first = created.handle;
    assert_eq!(open(system,
                    volume,
                    ROOT_ID,
                    DATA_FORK,
                    OPEN_WRITE | OPEN_CREATE_NEW,
                    "Std Native File").err(),
               Some(Error::AlreadyExists));
    write(system, first, b"abcdef")?;
    let clone = call(system, DUPLICATE, first)?.handle;
    seek(system, clone, 2)?;
    let mut pair = [0; 2];
    assert_eq!(read(system, first, &mut pair)?, 2);
    assert_eq!(&pair, b"cd");
    assert_eq!(call(system, TELL, clone)?.position, 4);
    call(system, CLOSE, first)?;
    write(system, clone, b"Z")?;
    system.log("STD_FILES_NATIVE_SHARED_CURSOR_CLOSE_OBSERVED\n")?;
    let append_a = open(system, volume, id, DATA_FORK, OPEN_APPEND, "")?.handle;
    let append_b = open(system, volume, id, DATA_FORK, OPEN_APPEND, "")?.handle;
    write(system, append_a, b"+")?;
    write(system, append_b, b"!")?;
    assert_eq!(call(system, TELL, clone)?.length, 8, "independent description live length");
    seek(system, clone, 0)?;
    let mut prefix = [0; 8];
    assert_eq!(read(system, clone, &mut prefix)?, 8);
    assert_eq!(&prefix, b"abcdZf+!");
    call(system, CLOSE, append_a)?;
    call(system, CLOSE, append_b)?;
    system.log("STD_FILES_NATIVE_INDEPENDENT_APPEND_OBSERVED\n")?;
    let resource = open(system, volume, id, RESOURCE_FORK, OPEN_READ | OPEN_WRITE, "")?.handle;
    write(system, resource, b"RSRC")?;
    seek(system, clone, 32)?;
    write(system, clone, b"Q")?;
    seek(system, clone, 8)?;
    let mut gap = [0x55; 25];
    assert_eq!(read(system, clone, &mut gap)?, 25);
    assert!(gap[..24].iter().all(|byte| *byte == 0));
    assert_eq!(gap[24], b'Q');
    length(system, clone, 3)?;
    length(system, clone, 17)?;
    assert_eq!(call(system, TELL, clone)?.position, 33, "resize changed shared cursor");
    assert_eq!(read(system, clone, &mut pair)?, 0, "past EOF read");
    seek(system, clone, 3)?;
    let mut grown = [0x55; 14];
    assert_eq!(read(system, clone, &mut grown)?, 14);
    assert_eq!(grown, [0; 14]);
    seek(system, resource, 0)?;
    let mut rsrc = [0; 4];
    assert_eq!(read(system, resource, &mut rsrc)?, 4);
    assert_eq!(&rsrc, b"RSRC");
    call(system, CLOSE, resource)?;
    system.log("STD_FILES_NATIVE_RESIZE_ZERO_RESOURCE_OBSERVED\n")?;
    let readonly = open(system, volume, id, DATA_FORK, OPEN_READ, "")?.handle;
    assert_eq!(write(system, readonly, b"X"), Err(Error::AccessDenied));
    call(system, CLOSE, readonly)?;
    length(system, clone, 4)?;
    seek(system, clone, 0)?;
    write(system, clone, b"SYNC")?;
    system.log("STD_FILES_NATIVE_SYNC_BEGIN\n")?;
    call(system, SYNC, clone)?;
    call(system, CLOSE, clone)?;
    assert_eq!(call(system, TELL, clone).err(), Some(Error::NotFound));
    system.log("STD_FILES_NATIVE_BOUNDED_CASES_OBSERVED\n")?;
    let _ = abi::NATIVE_LAYOUT_FINGERPRINT;
    Ok(())
}
mac_toolbox::application!(run);
