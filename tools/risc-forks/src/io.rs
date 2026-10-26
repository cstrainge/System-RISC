//! OS adapters own file handles; the format crate owns every byte-layout rule.
use std::{fs::File,
          io::{Read, Seek, SeekFrom, Write}};

use mac_fork_file::{Error, ReadAt, Result, WriteAt};
type Revision = [u8; 32];
fn revision(file: &File) -> std::io::Result<Revision>
{
    risc_host::file_revision(file).map_err(std::io::Error::other)
}
pub struct FileIo
{
    pub file: File,
    length:   u64,
    expected: Option<Revision>
}
impl FileIo
{
    pub fn new(file: File) -> std::io::Result<Self>
    {
        let length = file.metadata()?.len();
        Ok(Self { file,
                  length,
                  expected: None })
    }

    pub fn snapshot(file: File) -> std::io::Result<Self>
    {
        // Folder snapshots track metadata, while the volume's dedicated lock
        // record coordinates writers. Windows does not support byte locks on
        // directory handles.
        if file.metadata()?.is_file()
        {
            risc_host::try_lock_file(&file, false).map_err(std::io::Error::other)?;
        }
        let expected = revision(&file)?;
        Ok(Self { length: file.metadata()?.len(),
                  file,
                  expected: Some(expected) })
    }
}
impl ReadAt for FileIo
{
    fn len(&self) -> u64
    {
        self.length
    }

    fn validate(&self) -> Result<()>
    {
        if let Some(expected) = &self.expected
           && revision(&self.file).map_err(|_| Error::Io)? != *expected
        {
            return Err(Error::Stale);
        }
        Ok(())
    }

    fn read_exact(&mut self, offset: u64, output: &mut [u8]) -> Result<()>
    {
        if output.is_empty()
        {
            return Ok(());
        }
        if offset.checked_add(output.len() as u64).is_none_or(|end| end > self.length)
        {
            return Err(Error::Range);
        }
        self.file.seek(SeekFrom::Start(offset)).map_err(|_| Error::Io)?;
        self.file.read_exact(output).map_err(|_| Error::Io)
    }
}
impl WriteAt for FileIo
{
    fn set_len(&mut self, length: u64) -> Result<()>
    {
        if self.expected.is_some()
        {
            return Err(Error::Invalid);
        }
        self.file.set_len(length).map_err(|_| Error::Io)?;
        self.length = length;
        Ok(())
    }

    fn write_all(&mut self, offset: u64, input: &[u8]) -> Result<()>
    {
        if self.expected.is_some()
        {
            return Err(Error::Invalid);
        }
        if input.is_empty()
        {
            return Ok(());
        }
        if offset.checked_add(input.len() as u64).is_none_or(|end| end > self.length)
        {
            return Err(Error::Range);
        }
        self.file.seek(SeekFrom::Start(offset)).map_err(|_| Error::Io)?;
        self.file.write_all(input).map_err(|_| Error::Io)
    }

    fn flush(&mut self) -> Result<()>
    {
        self.file.sync_all().map_err(|_| Error::Io)
    }
}
pub struct Slice<'a>(pub &'a [u8]);
impl ReadAt for Slice<'_>
{
    fn len(&self) -> u64
    {
        self.0.len() as u64
    }

    fn read_exact(&mut self, offset: u64, output: &mut [u8]) -> Result<()>
    {
        let start = usize::try_from(offset).map_err(|_| Error::Range)?;
        let end = start.checked_add(output.len()).ok_or(Error::Range)?;
        output.copy_from_slice(self.0.get(start..end).ok_or(Error::Range)?);
        Ok(())
    }
}
