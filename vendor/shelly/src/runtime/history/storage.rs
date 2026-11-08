
use std::{ collections::HashSet,
           fs::{ File, OpenOptions, TryLockError, create_dir_all, remove_file, rename },
    io::{ self, BufRead, BufReader, BufWriter, Error, ErrorKind, Read, Write },
           path::{ Path, PathBuf },
           process::id,
           sync::atomic::{ AtomicU64, Ordering },
           thread::sleep,
           time::{ Duration, Instant } };

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use serde_json::{ Value, from_str, to_writer, json };

static TEMPORARIES: AtomicU64 = AtomicU64::new(0);

pub(super) struct Record
{
    pub id: String,
    pub command: String,
}

fn invalid(line: usize) -> Error
{
    Error::new(ErrorKind::InvalidData, format!("Invalid history record on line {}", line))
}

fn read(path: &Path, max_bytes: usize) -> io::Result<Vec<Record>>
{
    let file = match File::open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut records = Vec::new();
    let mut ids = HashSet::new();
    let file_limit = if max_bytes == usize::MAX { u64::MAX } else { 134217728 };
    let mut read_bytes = 0u64;
    for (number, line) in BufReader::new(file.take(file_limit)).lines().enumerate()
    {
        let number = number + 1;
        let line = line?;
        read_bytes = read_bytes.saturating_add(line.len() as u64 + 1);
        if max_bytes != usize::MAX && (read_bytes >= file_limit || number > 100000)
        { return Err(Error::new(ErrorKind::InvalidData, "History file exceeds storage limits")); }
        let value: Value = from_str(&line).map_err(|_| invalid(number))?;
        let id = value.get("id").and_then(Value::as_str).filter(|id| !id.is_empty());
        let command = value.get("command").and_then(Value::as_str)
            .filter(|command| !command.trim().is_empty());
        let (Some(id), Some(command)) = (id, command) else { return Err(invalid(number)); };
        if !ids.insert(id.to_string()) { return Err(invalid(number)); }
        records.push(Record { id: id.to_string(), command: command.to_string() });
    }
    Ok(records)
}

fn private_options() -> OpenOptions
{
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    options.mode(0o600);
    options
}

struct Temporary
{
    path: PathBuf,
    file: File,
}

impl Temporary
{
    fn new(path: &Path) -> io::Result<Self>
    {
        let parent = path.parent().unwrap();
        let name = path.file_name().unwrap().to_string_lossy();
        for _ in 0..100
        {
            let sequence = TEMPORARIES.fetch_add(1, Ordering::Relaxed);
            let temporary = parent.join(format!(".{}.{}-{}.tmp",
                name, id(), sequence));
            match private_options().create_new(true).open(&temporary)
            {
                Ok(file) => return Ok(Self { path: temporary, file }),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(Error::new(ErrorKind::AlreadyExists, "Cannot reserve history temporary file"))
    }
}

impl Drop for Temporary
{
    fn drop(&mut self) { let _ = remove_file(&self.path); }
}

fn write(path: &Path, records: &[Record]) -> io::Result<()>
{
    let temporary = Temporary::new(path)?;
    {
        let mut writer = BufWriter::new(&temporary.file);
        for record in records
        {
            let value = json!({"id": record.id, "command": record.command});
            to_writer(&mut writer, &value)
                .map_err(Error::other)?;
            writer.write_all(b"\n")?;
        }
        writer.flush()?;
    }
    temporary.file.sync_all()?;
    rename(&temporary.path, path)?;
    #[cfg(unix)]
    File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}

pub(super) fn synchronize(path: &Path, pending: &[Record], limit: usize, max_bytes: usize)
    -> io::Result<Vec<Record>>
{
    let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "History path must be absolute"))?;
    let name = path.file_name().ok_or_else(|| Error::new(ErrorKind::InvalidInput,
        "History path must name a file"))?;
    create_dir_all(parent)?;
    // A stable sidecar lock coordinates writers even as the history inode is replaced.
    let mut lock_name = name.to_os_string();
    lock_name.push(".lock");
    let lock = private_options().read(true).create(true).truncate(false)
        .open(parent.join(lock_name))?;
    let started = Instant::now();
    loop
    {
        match lock.try_lock()
        {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) if started.elapsed() < Duration::from_millis(200) =>
                sleep(Duration::from_millis(2)),
            Err(TryLockError::WouldBlock) => return Err(Error::new(ErrorKind::WouldBlock,
                "History file is locked by another process")),
            Err(TryLockError::Error(error)) => return Err(error),
        }
    }
    let mut records = read(path, max_bytes)?;
    let mut ids: HashSet<_> = records.iter().map(|record| record.id.as_str()).collect();
    let additions: Vec<_> = pending.iter().filter(|record| ids.insert(&record.id)).collect();
    let previous_length = records.len();
    let added = !additions.is_empty();
    records.extend(additions.into_iter().map(|record|
        Record { id: record.id.clone(), command: record.command.clone() }));
    // Deduplicate at the shared boundary too, including simultaneous identical submissions.
    records.dedup_by(|right, left| right.command == left.command);
    let changed = added || records.len() != previous_length
        || records.len() > limit || !path.exists();
    if records.len() > limit { records.drain(..records.len() - limit); }
    let mut bytes: usize = records.iter().map(|item| item.command.len()).sum();
    let mut remove = 0;
    while bytes > max_bytes && remove < records.len()
    { bytes -= records[remove].command.len(); remove += 1; }
    if remove != 0 { records.drain(..remove); }
    if changed || remove != 0 { write(path, &records)?; }
    // Closing the lock file releases the lock on both success and error.
    Ok(records)
}
