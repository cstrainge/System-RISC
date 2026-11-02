//! Real target application using the public SDK and actual FILE trap. These
//! finite workflows exercise ordinary std file and intentional GUI null I/O;
//! they do not qualify arbitrary files, providers, pressure or CLAP streams.

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
fn path(system: &System<'_>, operation: u32, name: &str, flags: u32, output: &mut [u8])
        -> ToolboxResult<FileCall>
{
    let mut call = FileCall { operation,
                              open_flags: flags,
                              ..Default::default() };
    system.client().file(&mut call, name, output)?;
    Ok(call)
}
fn path_cases(system: &System<'_>, volume: u32) -> ToolboxResult<()>
{
    let mut info = FileCall { operation: VOLUME_INFO,
                              volume,
                              ..Default::default() };
    system.client().file(&mut info, "", &mut [])?;
    let root = format!("/Volumes/{}", info.volume_info.name());
    let documents = format!("{root}/Documents");
    let mut bytes = [0; 4096];
    system.log("STD_FILES_NATIVE_PATH_CURRENT_BEGIN\n")?;
    assert_eq!(path(system, CURRENT_DIRECTORY, "", 0, &mut []).err(), Some(Error::BadArgument));
    let current = path(system, CURRENT_DIRECTORY, "", 0, &mut bytes)?;
    assert_eq!(&bytes[..current.transferred],
               documents.as_bytes(),
               "GUI launch directory is not the user Documents folder");
    system.log("STD_FILES_NATIVE_PATH_CURRENT_OBSERVED\n")?;
    let parent = path(system, RESOLVE_PATH, ".././..", 0, &mut [])?;
    assert_eq!((parent.volume, parent.cnid), (volume, ROOT_ID));
    system.log("STD_FILES_NATIVE_PATH_PARENT_OBSERVED\n")?;
    path(system, CHANGE_DIRECTORY, "..", 0, &mut [])?;
    let current = path(system, CURRENT_DIRECTORY, "", 0, &mut bytes)?;
    assert_eq!(&bytes[..current.transferred], root.as_bytes());
    system.log("STD_FILES_NATIVE_PATH_ROOT_OBSERVED\n")?;
    path(system, CHANGE_DIRECTORY, &documents, 0, &mut [])?;
    system.log("STD_FILES_NATIVE_PATH_RESTORED_OBSERVED\n")?;
    let opened = path(system,
                      OPEN_PATH,
                      "../Std Path File",
                      OPEN_READ | OPEN_WRITE | OPEN_CREATE | OPEN_TRUNCATE,
                      &mut [])?;
    write(system, opened.handle, b"PATH")?;
    call(system, SYNC, opened.handle)?;
    call(system, CLOSE, opened.handle)?;
    let opened = path(system, OPEN_PATH, &format!("{root}/Std Path File"), OPEN_READ, &mut [])?;
    let mut saved = [0; 4];
    assert_eq!(read(system, opened.handle, &mut saved)?, 4);
    assert_eq!(&saved, b"PATH");
    call(system, CLOSE, opened.handle)?;
    assert_eq!(path(system, CHANGE_DIRECTORY, "../Std Path File", 0, &mut []).err(),
               Some(Error::BadArgument));
    assert_eq!(path(system, CHANGE_DIRECTORY, "No Such Native Directory", 0, &mut []).err(),
               Some(Error::NotFound));
    assert_eq!(path(system, RESOLVE_PATH, "bad\0name", 0, &mut []).err(), Some(Error::BadArgument));
    let current = path(system, CURRENT_DIRECTORY, "", 0, &mut bytes)?;
    assert_eq!(&bytes[..current.transferred],
               documents.as_bytes(),
               "failed directory change replaced old alias");
    assert_eq!(path(system,OPEN_PATH,"../Std Path File",OPEN_WRITE|OPEN_CREATE_NEW,&mut []).err(),Some(Error::AlreadyExists));
    system.log("STD_FILES_NATIVE_PATH_CWD_OPEN_REFUSAL_REUSE_OBSERVED\n")?;
    Ok(())
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
    path_cases(system, volume)?;
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
    assert_eq!(call(system, TELL, first)?.entry.id,
               id,
               "descriptor metadata must use its canonical live file");
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
fn main() -> Result<(), Box<dyn std::error::Error>>
{
    let system = System::new().expect("native diagnostic has installed platform services");
    run(&system).expect("finite native file/path cases");
    launch_context_cases(&system).expect("finite real std launch-context workflow");
    std_cases(&system).expect("finite real std File/BufReader/BufWriter workflow");
    null_stdio_cases(&system).expect("GUI null standard streams");
    if cfg!(feature = "result-error")
    {
        system.log("STD_GUI_RESULT_ERROR_BEGIN\n")?;
        return Err(std::io::Error::other("intentional ordinary GUI main error").into());
    }
    Ok(())
}

fn std_cases(system: &System<'_>) -> Result<(), Box<dyn std::error::Error>>
{
    use std::{fs::{self, File, OpenOptions},
              io::{BufRead, BufReader, BufWriter, ErrorKind, Read, Seek, SeekFrom, Write}};
    let original = std::env::current_dir()?;
    assert!(original.ends_with("Documents"));
    let path = original.join("Std Rust File");
    if path.try_exists()?
    {
        assert_eq!(fs::read_to_string(&path)?,
                   "STD α\n",
                   "std-written bytes did not survive the fresh guest boot");
        system.log("STD_FILES_ACTUAL_STD_REBOOT_BYTES_OBSERVED\n")?;
    }
    let mut file =
        OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&path)?;
    file.write_all(b"abcdef")?;
    let mut cloned = file.try_clone()?;
    cloned.seek(SeekFrom::Start(2))?;
    let mut pair = [0; 2];
    file.read_exact(&mut pair)?;
    assert_eq!(&pair, b"cd");
    drop(file);
    cloned.write_all(b"Z")?;
    assert_eq!(cloned.metadata()?.len(), 6);
    assert!(cloned.metadata()?.is_file());
    assert!(cloned.metadata()?.modified()?.duration_since(std::time::UNIX_EPOCH).is_ok());
    let mut append = OpenOptions::new().append(true).open(&path)?;
    append.write_all(b"+")?;
    drop(append);
    cloned.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    cloned.read_to_end(&mut bytes)?;
    assert_eq!(&bytes, b"abcdZf+");
    let cursor = cloned.stream_position()?;
    cloned.set_len(3 * MAX_FILE_TRANSFER as u64 + 19)?;
    assert_eq!(cloned.stream_position()?, cursor);
    cloned.seek(SeekFrom::Start(MAX_FILE_TRANSFER as u64 + 7))?;
    let mut zeros = [0x55; 32];
    cloned.read_exact(&mut zeros)?;
    assert_eq!(zeros, [0; 32]);
    assert_eq!(cloned.seek(SeekFrom::Start(u64::MAX)).unwrap_err().kind(), ErrorKind::InvalidInput);
    cloned.set_len(4)?;
    cloned.seek(SeekFrom::Start(0))?;
    {
        let mut writer = BufWriter::with_capacity(5, &cloned);
        writer.write_all("STD α\n".as_bytes())?;
        writer.flush()?;
    }
    cloned.sync_data()?;
    cloned.sync_all()?;
    drop(cloned);
    assert_eq!(fs::read_to_string(&path)?, "STD α\n");
    let mut reader = BufReader::with_capacity(5, File::open(&path)?);
    assert_eq!(reader.fill_buf()?,
               b"STD \xCE",
               "diagnostic must split the UTF-8 scalar across actual reads");
    let mut text = String::new();
    reader.read_to_string(&mut text)?;
    assert_eq!(text, "STD α\n");
    drop(reader);
    fs::write(&path, [0xff])?;
    assert_eq!(fs::read_to_string(&path).unwrap_err().kind(), ErrorKind::InvalidData);
    assert_eq!(fs::read(&path)?, [0xff]);
    fs::write(&path, "STD α\n")?;
    File::open(&path)?.sync_all()?;
    assert!(fs::metadata(&original)?.is_dir());
    assert!(path.try_exists()?);
    assert_eq!(OpenOptions::new().write(true).create_new(true).open(&path).unwrap_err().kind(),
               ErrorKind::AlreadyExists);
    assert_eq!(File::open(original.join("No Such Std File")).unwrap_err().kind(),
               ErrorKind::NotFound);
    let mut read_only = File::open(&path)?;
    assert_eq!(read_only.write(b"!").unwrap_err().kind(), ErrorKind::PermissionDenied);
    drop(read_only);
    std::env::set_current_dir("..")?;
    assert_eq!(fs::read_to_string("Documents/Std Rust File")?, "STD α\n");
    assert_eq!(std::env::set_current_dir("No Such Std Folder").unwrap_err().kind(),
               ErrorKind::NotFound);
    assert_ne!(std::env::current_dir()?, original);
    std::env::set_current_dir(&original)?;
    assert_eq!(fs::read_to_string("Std Rust File")?, "STD α\n");
    system.log("STD_FILES_ACTUAL_STD_FILE_BUFFER_CWD_REFUSAL_REUSE_OBSERVED\n")?;
    Ok(())
}

fn null_stdio_cases(system: &System<'_>) -> Result<(), Box<dyn std::error::Error>>
{
    use std::io::{self, IsTerminal, Read, Write};
    let mut bytes = [0x55; 16];
    assert_eq!(io::stdin().read(&mut bytes)?, 0);
    assert_eq!(bytes, [0x55; 16], "null stdin changed its caller's buffer");
    let mut text = String::new();
    assert_eq!(io::stdin().read_to_string(&mut text)?, 0);
    assert!(text.is_empty());
    let arbitrary = [0xff, 0x80, 0, 0xce, 0xb1, b'\n'];
    assert_eq!(io::stdout().write(&arbitrary)?, arbitrary.len());
    io::stdout().flush()?;
    assert_eq!(io::stderr().write(&arbitrary)?, arbitrary.len());
    io::stderr().flush()?;
    println!("intentional GUI null stdout");
    eprintln!("intentional GUI null stderr");
    io::stdout().flush()?;
    io::stderr().flush()?;
    assert!(!io::stdin().is_terminal());
    assert!(!io::stdout().is_terminal());
    assert!(!io::stderr().is_terminal());
    system.log("STD_GUI_NULL_STDIN_STDOUT_STDERR_OBSERVED\n")?;
    Ok(())
}

fn launch_context_cases(system: &System<'_>) -> Result<(), Box<dyn std::error::Error>>
{
    use std::{env,
              sync::atomic::{AtomicBool, Ordering}};
    assert_eq!(env::args_os().len(), 0, "GUI launch fabricated command arguments");
    assert_eq!(env::vars_os().count(), 0, "GUI launch imported an environment");
    let executable = env::current_exe()?;
    assert!(executable.ends_with("Applications/Panic Test"));
    assert!(executable.is_absolute());
    assert!(std::path::Path::new("/").is_absolute());
    assert!(std::path::Path::new("Documents").is_relative());
    assert_eq!(std::path::Path::new("/ignored").join(&executable), executable);
    assert!(std::fs::metadata(&executable)?.is_file());
    let entry = path(system, RESOLVE_PATH, executable.to_str().unwrap(), 0, &mut [])?;
    let mut remove = FileCall { operation: REMOVE,
                                volume: entry.volume,
                                cnid: entry.cnid,
                                ..Default::default() };
    assert_eq!(system.client().file(&mut remove, "", &mut []).err(),
               Some(Error::ServiceBusy),
               "native removal did not retain the busy executing file");
    assert!(std::fs::metadata(&executable)?.is_file());
    let documents = env::current_dir()?;
    assert_eq!(std::path::absolute("Std Rust File")?, documents.join("Std Rust File"));
    env::set_current_dir("..")?;
    assert_eq!(env::current_exe()?, executable, "chdir changed executable identity");
    env::set_current_dir(&documents)?;
    // These operations exercise the platform's synchronized process-local
    // environment. No C/libc/foreign environment readers exist in this profile.
    unsafe {
        env::set_var("STD_GUI_α", "first value α");
    }
    let snapshot = env::vars();
    unsafe {
        env::set_var("STD_GUI_α", "second value β");
    }
    assert_eq!(snapshot.collect::<Vec<_>>(),
               [("STD_GUI_α".to_owned(), "first value α".to_owned())]);
    assert_eq!(env::var("STD_GUI_α")?, "second value β");
    let ready = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let ready_ref = &ready;
        let expected = &executable;
        let worker = scope.spawn(move || {
                              assert_eq!(env::current_exe().unwrap(), *expected);
                              for n in 0..32
                              {
                                  unsafe {
                                      env::set_var("STD_GUI_WORKER", format!("worker-{n}-α"));
                                  }
                              }
                              ready_ref.store(true, Ordering::Release);
                          });
        for _ in 0..64
        {
            let values = env::vars().collect::<Vec<_>>();
            assert_eq!(values.iter().filter(|(key, _)| key == "STD_GUI_α").count(), 1);
            assert!(values.iter().all(|(key, value)| {
                                     key != "STD_GUI_WORKER"
                                     || value.starts_with("worker-") && value.ends_with("-α")
                                 }));
        }
        worker.join().unwrap();
        assert!(ready.load(Ordering::Acquire));
    });
    assert_eq!(env::var("STD_GUI_WORKER")?, "worker-31-α");
    unsafe {
        env::remove_var("STD_GUI_α");
        env::remove_var("STD_GUI_WORKER");
    }
    assert!(env::var_os("STD_GUI_α").is_none());
    assert_eq!(env::vars_os().count(), 0);
    assert_eq!(env::current_exe()?, executable);
    system.log("STD_GUI_ACTUAL_EXE_PIN_ARGS_ENV_SNAPSHOT_WORKER_REUSE_OBSERVED\n")?;
    Ok(())
}
