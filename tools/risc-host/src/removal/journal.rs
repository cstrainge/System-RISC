//! Two checksummed slots retain a complete intent through torn phase writes.
//! Journals live beside their source, so directory renames need no path repair.
use super::*;

pub(crate) const PREFIX: &str = ".system-risc-delete-";
const SLOT: usize = 1024;
const HASH: usize = SLOT - 32;
const MAGIC: &[u8; 8] = b"RSHDEL01";

pub(super) struct Journal
{
    pub name: String,
    file:     File,
    bytes:    [u8; SLOT],
    slot:     usize
}
pub(super) struct Intent<'a>
{
    pub phase:       u32,
    pub parent:      wire::ObjectIdentity,
    pub source:      wire::ObjectIdentity,
    pub record:      Option<wire::ObjectIdentity>,
    pub original:    &'a str,
    pub hidden:      &'a str,
    pub record_name: Option<&'a str>
}
fn identity(file: &File) -> Result<wire::ObjectIdentity>
{
    object_identity(&file)?.ok_or(Failure(status::UNSUPPORTED))
}
fn seal(bytes: &mut [u8; SLOT])
{
    let digest = Sha256::digest(&bytes[..HASH]);
    bytes[HASH..].copy_from_slice(&digest);
}
fn decode(bytes: &[u8; SLOT]) -> Result<Intent<'_>>
{
    let invalid = || Failure(status::CORRUPT);
    if &bytes[..8] != MAGIC
       || bytes[8..12] != 1u32.to_le_bytes()
       || bytes[118..120] != [0; 2]
       || Sha256::digest(&bytes[..HASH]).as_slice() != &bytes[HASH..]
    {
        return Err(invalid());
    }
    let phase = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
    if ![wire::REMOVE_PREPARED,
         wire::REMOVE_QUARANTINED,
         wire::REMOVE_COMMITTING,
         wire::REMOVE_ABORTED].contains(&phase)
    {
        return Err(invalid());
    }
    let parent =
        wire::ObjectIdentity::from_bytes(bytes[16..48].try_into().unwrap()).map_err(|_| invalid())?;
    let source =
        wire::ObjectIdentity::from_bytes(bytes[48..80].try_into().unwrap()).map_err(|_| invalid())?;
    let lengths: [usize; 3] = core::array::from_fn(|i| {
        u16::from_le_bytes(bytes[112 + i * 2..114 + i * 2].try_into().unwrap()) as usize
    });
    let end = 128 + lengths.iter().sum::<usize>();
    if lengths.iter().any(|n| *n > 255) || end > HASH || bytes[end..HASH].iter().any(|b| *b != 0)
    {
        return Err(invalid());
    }
    let original = wire::component(&bytes[128..128 + lengths[0]]).map_err(|_| invalid())?;
    let hidden_at = 128 + lengths[0];
    let hidden = wire::component(&bytes[hidden_at..hidden_at + lengths[1]]).map_err(|_| invalid())?;
    if !hidden.starts_with(".system-risc-stage-")
    {
        return Err(invalid());
    }
    let record_name = if lengths[2] == 0
    {
        None
    }
    else
    {
        Some(wire::component(&bytes[hidden_at + lengths[1]..end]).map_err(|_| invalid())?)
    };
    let record_bytes: [u8; 32] = bytes[80..112].try_into().unwrap();
    let record = if record_name.is_some()
    {
        Some(wire::ObjectIdentity::from_bytes(record_bytes).map_err(|_| invalid())?)
    }
    else
    {
        if record_bytes != [0; 32]
        {
            return Err(invalid());
        }
        None
    };
    Ok(Intent { phase,
                parent,
                source,
                record,
                original,
                hidden,
                record_name })
}
impl Journal
{
    pub fn create(parent: &Dir, original: &str, hidden: &str, source: &File,
                  record: Option<(&str, &File)>)
                  -> Result<Self>
    {
        let mut bytes = [0; SLOT];
        bytes[..8].copy_from_slice(MAGIC);
        bytes[8..12].copy_from_slice(&1u32.to_le_bytes());
        bytes[16..48].copy_from_slice(identity(&pin_directory_file(&parent)?)?.as_bytes());
        bytes[48..80].copy_from_slice(identity(source)?.as_bytes());
        if let Some((_, file)) = record
        {
            bytes[80..112].copy_from_slice(identity(file)?.as_bytes());
        }
        let mut at = 128;
        for (i, name) in
            [original, hidden, record.map_or("", |(name, _)| name)].into_iter().enumerate()
        {
            if !name.is_empty()
            {
                wire::component(name.as_bytes())?;
            }
            bytes[112 + i * 2..114 + i * 2].copy_from_slice(&(name.len() as u16).to_le_bytes());
            bytes[at..at + name.len()].copy_from_slice(name.as_bytes());
            at += name.len();
        }
        seal(&mut bytes);
        decode(&bytes)?;
        let name = unique()?.replacen(".system-risc-stage-", PREFIX, 1);
        let mut file =
            parent.open_with(&name, OpenOptions::new().read(true).write(true).create_new(true))?
                  .into_std();
        let result: Result<()> = (|| {
            try_lock_file(&file, true)?;
            file.write_all(&bytes)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            sync_directory(parent)
        })();
        if let Err(error) = result
        {
            // No source namespace operation has been admitted yet.
            if let Ok(current) = parent.open(&name)
               && same_object(&current.into_std(), &file).unwrap_or(false)
            {
                let _ = remove_file(&parent, &name);
            }
            let _ = sync_directory(parent);
            return Err(error);
        }
        Ok(Self { name,
                  file,
                  bytes,
                  slot: 0 })
    }

    pub fn open(parent: &Dir, name: &str) -> Result<Self>
    {
        if !name.starts_with(PREFIX)
        {
            return Err(Failure(status::INVALID));
        }
        wire::component(name.as_bytes())?;
        if !parent.symlink_metadata(name)?.is_file()
        {
            return Err(Failure(status::CORRUPT));
        }
        let mut file =
            parent.open_with(name, OpenOptions::new().read(true).write(true))?.into_std();
        try_lock_file(&file, true)?;
        if file.metadata()?.len() != (2 * SLOT) as u64
        {
            return Err(Failure(status::CORRUPT));
        }
        let mut first = [0; SLOT];
        let mut second = [0; SLOT];
        file.read_exact(&mut first)?;
        file.read_exact(&mut second)?;
        let sequence = |bytes: &[u8; SLOT]| u64::from_le_bytes(bytes[120..128].try_into().unwrap());
        let slot = match (decode(&first).is_ok(), decode(&second).is_ok())
        {
            (true, true) => usize::from(sequence(&second) > sequence(&first)),
            (true, false) => 0,
            (false, true) => 1,
            (false, false) => return Err(Failure(status::CORRUPT))
        };
        let bytes = if slot == 0 { first } else { second };
        let intent = decode(&bytes)?;
        if intent.parent != identity(&pin_directory_file(&parent)?)?
        {
            return Err(Failure(status::CONFLICT));
        }
        Ok(Self { name: name.into(),
                  file,
                  bytes,
                  slot })
    }

    pub fn intent(&self) -> Result<Intent<'_>>
    {
        decode(&self.bytes)
    }

    pub fn phase(&mut self, phase: u32) -> Result<()>
    {
        if u32::from_le_bytes(self.bytes[12..16].try_into().unwrap()) == phase
        {
            return Ok(());
        }
        let mut bytes = self.bytes;
        let sequence =
            u64::from_le_bytes(bytes[120..128].try_into().unwrap()).checked_add(1)
                                                                   .ok_or(Failure(status::LIMIT))?;
        bytes[12..16].copy_from_slice(&phase.to_le_bytes());
        bytes[120..128].copy_from_slice(&sequence.to_le_bytes());
        seal(&mut bytes);
        decode(&bytes)?;
        let slot = 1 - self.slot;
        self.file.seek(SeekFrom::Start((slot * SLOT) as u64))?;
        self.file.write_all(&bytes)?;
        self.file.sync_all()?;
        self.bytes = bytes;
        self.slot = slot;
        Ok(())
    }

    pub fn cleanup(&self, parent: &Dir) -> Result<()>
    {
        match parent.open(&self.name)
        {
            Ok(file) =>
            {
                if !same_object(&file.into_std(), &self.file)?
                {
                    return Err(Failure(status::CONFLICT));
                }
                remove_file(&parent, &self.name)?;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into())
        }
        sync_directory(parent)
    }
}
