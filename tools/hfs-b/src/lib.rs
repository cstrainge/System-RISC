//! Thin host storage/import adapters over the same filesystem used by System.
use std::{fs::{File, OpenOptions},
          io::{Read, Seek, SeekFrom, Write},
          path::Path};

use mac_hfs_b::{time::Timestamp, *};
pub mod copy;
pub type HostResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub struct Image
{
    file:      File,
    sectors:   u64,
    read_only: bool
}
impl Image
{
    pub fn open(path: &Path, writable: bool) -> HostResult<Self>
    {
        let mut options = OpenOptions::new();
        options.read(true).write(writable);
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
            if std::fs::metadata(path)?.file_type().is_block_device() && writable
            {
                // Linux block-device O_EXCL refuses a device claimed by a
                // mounted filesystem. No unsafe ioctl is needed for geometry.
                options.custom_flags(0x80);
            }
        }
        let mut file = options.open(path)?;
        if writable
        {
            file.try_lock()?;
        }
        else
        {
            file.try_lock_shared()?;
        }
        let bytes = file.seek(SeekFrom::End(0))?;
        if bytes == 0 || !bytes.is_multiple_of(512)
        {
            return Err("image size must be a nonzero sector multiple".into());
        }
        Ok(Self { file,
                  sectors: bytes / 512,
                  read_only: !writable })
    }

    pub fn create(path: &Path, bytes: u64) -> HostResult<Self>
    {
        if bytes < 65536 || !bytes.is_multiple_of(4096)
        {
            return Err("image size must be at least 64 KiB and a 4096-byte multiple".into());
        }
        let file = OpenOptions::new().read(true).write(true).create_new(true).open(path)?;
        file.try_lock()?;
        file.set_len(bytes)?;
        Ok(Self { file,
                  sectors: bytes / 512,
                  read_only: false })
    }

    pub fn partition(&self) -> Partition
    {
        Partition { start:   0,
                    sectors: self.sectors }
    }
}
impl BlockDevice for Image
{
    fn sector_count(&self) -> u64
    {
        self.sectors
    }

    fn read_sector(&mut self, sector: u64, out: &mut [u8; 512]) -> mac_block::Result<()>
    {
        if sector >= self.sectors
        {
            return Err(mac_block::Error("host read outside device"));
        }
        self.file
            .seek(SeekFrom::Start(sector * 512))
            .map_err(|_| mac_block::Error("host image seek failed"))?;
        self.file.read_exact(out).map_err(|_| mac_block::Error("host image read failed"))
    }
}
impl WritableBlockDevice for Image
{
    fn is_read_only(&self) -> bool
    {
        self.read_only
    }

    fn write_sector(&mut self, sector: u64, input: &[u8; 512]) -> mac_block::Result<()>
    {
        if self.read_only || sector >= self.sectors
        {
            return Err(mac_block::Error("host write unavailable/outside device"));
        }
        self.file
            .seek(SeekFrom::Start(sector * 512))
            .map_err(|_| mac_block::Error("host image seek failed"))?;
        self.file.write_all(input).map_err(|_| mac_block::Error("host image write failed"))
    }

    fn flush(&mut self) -> mac_block::Result<()>
    {
        self.file.sync_all().map_err(|_| mac_block::Error("host image flush failed"))
    }
}
pub fn now() -> HostResult<Timestamp>
{
    let duration = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?;
    Ok(Timestamp::from_unix(i64::try_from(duration.as_secs())?, duration.subsec_micros())?)
}
pub fn import_macbinary<D: WritableBlockDevice>(volume: &mut Volume<'_, D>, parent: u32,
                                                file: &mac_formats::macbinary::MacFile)
                                                -> HostResult<Entry>
{
    import_macbinary_named(volume, parent, file, &mac_formats::text::decode(&file.name))
}
pub fn import_macbinary_named<D: WritableBlockDevice>(volume: &mut Volume<'_, D>, parent: u32,
                                                      file: &mac_formats::macbinary::MacFile,
                                                      name: &str)
                                                      -> HostResult<Entry>
{
    import_macbinary_at(volume, parent, file, name, now()?)
}
/// Import file dates verbatim while timestamping destination changes separately.
pub fn import_macbinary_at<D: WritableBlockDevice>(volume: &mut Volume<'_, D>, parent: u32,
                                                   file: &mac_formats::macbinary::MacFile,
                                                   name: &str, when: Timestamp)
                                                   -> HostResult<Entry>
{
    let e = volume.create(parent, name, false, when)?;
    for (kind, data) in [(ForkKind::Data, &file.data), (ForkKind::Resource, &file.resources)]
    {
        for (index, chunk) in data.chunks(64 * 1024).enumerate()
        {
            volume.write_fork(e.id, kind, (index * 64 * 1024) as u64, chunk, when)?;
        }
    }
    let mut info = [0; 32];
    info[..4].copy_from_slice(&file.file_type);
    info[4..8].copy_from_slice(&file.creator);
    info[8..10].copy_from_slice(&file.flags.to_be_bytes());
    Ok(volume.set_metadata(e.id,
                           0,
                           info,
                           Timestamp::from_legacy(file.created),
                           Timestamp::from_legacy(file.modified),
                           None,
                           when)?)
}
pub fn write_stream<D: WritableBlockDevice>(volume: &mut Volume<'_, D>, id: u32, kind: ForkKind,
                                            mut input: impl Read, when: Timestamp)
                                            -> HostResult<u64>
{
    let mut buffer = [0; 64 * 1024];
    let mut position = 0;
    loop
    {
        let n = input.read(&mut buffer)?;
        if n == 0
        {
            break;
        }
        volume.write_fork(id, kind, position, &buffer[..n], when)?;
        position += n as u64;
    }
    Ok(position)
}
pub fn read_stream<D: BlockDevice>(volume: &mut Volume<'_, D>, id: u32, kind: ForkKind,
                                   mut output: impl Write)
                                   -> HostResult<u64>
{
    let mut buffer = [0; 64 * 1024];
    let mut position = 0;
    loop
    {
        let n = volume.read_fork(id, kind, position, &mut buffer)?;
        if n == 0
        {
            break;
        }
        output.write_all(&buffer[..n])?;
        position += n as u64;
    }
    Ok(position)
}
