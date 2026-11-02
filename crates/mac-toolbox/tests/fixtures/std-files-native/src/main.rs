//! Real target application using the public SDK and actual FILE trap. These
//! finite observations do not qualify std, arbitrary large files or pressure.
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
    assert_eq!(call.length, target, "native resize did not reach requested target");
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
    system.log("STD_FILES_NATIVE_RESIZE_ZERO_RESOURCE_OBSERVED\n")?;
    // Each request exceeds the resident device-step bound. Application code
    // issues one FILE request, without SDK resize/write continuation loops.
    const LARGE: u64 = 3 * MAX_FILE_TRANSFER as u64 + 19;
    length(system, clone, LARGE)?;
    assert_eq!(call(system, TELL, clone)?.position, 17, "large growth moved cursor");
    seek(system, clone, (MAX_FILE_TRANSFER as u64 + 7) as i64)?;
    let mut zero = [0x55; 32];
    assert_eq!(read(system, clone, &mut zero)?, zero.len());
    assert_eq!(zero, [0; 32]);
    seek(system, clone, (LARGE + 2 * MAX_FILE_TRANSFER as u64 + 13) as i64)?;
    let gap_position = call(system, TELL, clone)?.position;
    write(system, clone, &[])?;
    assert_eq!(call(system, TELL, clone)?.length, LARGE, "empty write grew gap");
    write(system, clone, b"GAP")?;
    let after = call(system, TELL, clone)?;
    assert_eq!(after.position, gap_position + 3);
    assert_eq!(after.length, gap_position + 3);
    seek(system, clone, (gap_position - 8) as i64)?;
    let mut tail = [0x55; 11];
    assert_eq!(read(system, clone, &mut tail)?, tail.len());
    assert_eq!(&tail[..8], &[0; 8]);
    assert_eq!(&tail[8..], b"GAP");
    length(system, clone, 5)?;
    assert_eq!(call(system, TELL, clone)?.position, gap_position + 3, "large shrink moved cursor");
    assert_eq!(read(system, clone, &mut tail)?, 0);
    seek(system, resource, 0)?;
    assert_eq!(read(system, resource, &mut rsrc)?, 4);
    assert_eq!(&rsrc, b"RSRC");
    system.log("STD_FILES_NATIVE_CONTINUATION_GROW_GAP_SHRINK_OBSERVED\n")?;
    length(system, clone, LARGE)?;
    let truncated =
        open(system, volume, id, DATA_FORK, OPEN_READ | OPEN_WRITE | OPEN_TRUNCATE, "")?;
    assert_eq!(truncated.length, 0, "single open truncate did not finish");
    assert_eq!(call(system, TELL, clone)?.length, 0, "independent descriptor kept old length");
    assert_eq!(call(system, TELL, clone)?.position,
               gap_position + 3,
               "open truncate moved another cursor");
    seek(system, resource, 0)?;
    assert_eq!(read(system, resource, &mut rsrc)?, 4);
    assert_eq!(&rsrc, b"RSRC");
    write(system, truncated.handle, b"new")?;
    call(system, CLOSE, truncated.handle)?;
    length(system, clone, 5)?;
    system.log("STD_FILES_NATIVE_PREPARED_OPEN_TRUNCATE_OBSERVED\n")?;
    call(system, CLOSE, resource)?;
    let readonly = open(system, volume, id, DATA_FORK, OPEN_READ, "")?.handle;
    assert_eq!(write(system, readonly, b"X"), Err(Error::AccessDenied));
    call(system, CLOSE, readonly)?;
    // Exercise invalid canonical handles with a full-sized payload. These
    // refusals precede Subject/staging allocation in the actual FILE path.
    let refused = [0x5a; MAX_FILE_TRANSFER];
    assert_eq!(write(system, readonly, &refused), Err(Error::NotFound));
    let readonly_again = open(system, volume, id, DATA_FORK, OPEN_READ, "")?.handle;
    assert_eq!(write(system, readonly_again, &refused), Err(Error::AccessDenied));
    assert_eq!(call(system, TELL, clone)?.length, 5, "rejected write changed live file");
    call(system, CLOSE, readonly_again)?;
    system.log("STD_FILES_NATIVE_PREFLIGHT_REFUSAL_REUSE_OBSERVED\n")?;
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
