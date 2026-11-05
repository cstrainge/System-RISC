//! Writing a platform image to a physical medium.
//!
//! This destroys the target's existing contents, so the rules are absolute: the
//! operator names the device, nothing is written without an explicit `--yes`, and
//! the image always comes from the platform record rather than a path argument.
//!
//! The write and verify path is a library over `Write + Seek` and `Read + Seek`,
//! so it is exercised byte for byte against a temporary file in host tests.
//! Opening a raw device is the one platform-specific step and lives in `device`,
//! so no raw-device call appears in the command handler.

use std::io::{Read, Seek, SeekFrom, Write};

use crate::{Result, error};

pub mod device;

/// What a flash would do. Built before anything is written, so the operator sees
/// the sizes and the existing layout first.
#[derive(Debug)]
pub struct Plan
{
    pub target:          String,
    pub target_capacity: u64,
    pub image_bytes:     u64,
    pub existing:        Vec<String>
}

impl Plan
{
    /// Refuses anything that would fail or destroy more than intended, before the
    /// first byte is written. `capacity` is the target's size in bytes.
    pub fn new(target: &str, capacity: u64, image_bytes: u64, existing: Vec<String>)
               -> Result<Self>
    {
        if image_bytes == 0
        {
            return Err(error("the image is empty; nothing would be written"));
        }
        if image_bytes > capacity
        {
            return Err(error(format!("the image is {image_bytes} bytes but {target} holds only \
                                      {capacity} bytes; writing it would truncate the image")));
        }
        Ok(Self { target: target.to_string(),
                  target_capacity: capacity,
                  image_bytes,
                  existing })
    }

    /// What the operator is about to lose, in one block.
    pub fn describe(&self) -> String
    {
        let mut text = format!("target:  {}\n  capacity: {} bytes\n  image:    {} bytes\n",
                               self.target, self.target_capacity, self.image_bytes);
        if self.existing.is_empty()
        {
            text.push_str("  existing: no partitions or volumes reported\n");
        }
        else
        {
            text.push_str("  existing contents, which will be erased:\n");
            for entry in &self.existing
            {
                text.push_str(&format!("    {entry}\n"));
            }
        }
        text
    }
}

/// Writes the whole image from the start of the sink.
pub fn write_image(sink: &mut impl Write, image: &[u8]) -> Result<()>
{
    sink.write_all(image).map_err(|cause| error(format!("writing the image failed: {cause}")))?;
    sink.flush().map_err(|cause| error(format!("flushing the image failed: {cause}")))?;
    Ok(())
}

/// Re-reads the written region and compares it with the image.
///
/// This is the difference between "the write call returned" and "the medium holds
/// the image": a truncated or failed write is reported here rather than assumed
/// good because `write_all` succeeded.
pub fn verify(sink: &mut (impl Read + Seek), image: &[u8]) -> Result<()>
{
    sink.seek(SeekFrom::Start(0))
        .map_err(|cause| error(format!("seeking to verify failed: {cause}")))?;
    let mut buffer = vec![0u8; image.len()];
    sink.read_exact(&mut buffer)
        .map_err(|cause| error(format!("reading back {} bytes failed: {cause}", image.len())))?;
    if buffer != image
    {
        let offset = buffer.iter()
                           .zip(image.iter())
                           .position(|(written, expected)| written != expected)
                           .unwrap_or(0);
        return Err(error(format!("verification failed: the medium differs from the image at byte \
                                  {offset}")));
    }
    Ok(())
}

/// Writes the image and verifies it, in that order.
pub fn flash(sink: &mut (impl Read + Write + Seek), image: &[u8]) -> Result<()>
{
    write_image(sink, image)?;
    verify(sink, image)
}

#[cfg(test)]
mod tests
{
    use super::*;
    use std::{fs::OpenOptions, io::Cursor};

    /// A temp file that behaves like a medium: seekable, readable, writable.
    fn temp(name: &str) -> std::path::PathBuf
    {
        let path = std::env::temp_dir().join(format!("sr-flash-{}-{name}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn a_round_trip_writes_the_image_and_verifies_it_byte_for_byte()
    {
        let image: Vec<u8> = (0..=255u8).cycle().take(64 * 1024).collect();
        let path = temp("roundtrip");
        let mut file = OpenOptions::new().read(true)
                                         .write(true)
                                         .create(true)
                                         .truncate(true)
                                         .open(&path)
                                         .unwrap();
        flash(&mut file, &image).unwrap();
        let read_back = std::fs::read(&path).unwrap();
        assert_eq!(read_back, image, "the medium must hold exactly the image");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_truncated_write_is_reported_rather_than_assumed_good()
    {
        // A sink that silently accepts only part of the image is what a failing
        // device looks like; verification has to catch it.
        struct Truncating(usize);
        impl Write for Truncating
        {
            fn write(&mut self, data: &[u8]) -> std::io::Result<usize>
            {
                let take = data.len().min(self.0);
                self.0 -= take;
                Ok(take)
            }

            fn flush(&mut self) -> std::io::Result<()>
            {
                Ok(())
            }
        }
        let image = vec![7u8; 4096];
        let mut sink = Truncating(1024);
        let error = write_image(&mut sink, &image).expect_err("a short write must fail");
        assert!(error.to_string().contains("writing the image failed"), "{error}");
    }

    #[test]
    fn verification_names_the_first_differing_byte()
    {
        let image = vec![1u8; 512];
        let mut medium = Cursor::new(vec![1u8; 512]);
        medium.get_mut()[100] = 9;
        let error = verify(&mut medium, &image).expect_err("a differing medium must fail");
        assert!(error.to_string().contains("at byte 100"), "{error}");
    }

    #[test]
    fn a_plan_refuses_an_image_larger_than_the_target()
    {
        let error = Plan::new("disk", 1024, 2048, Vec::new()).expect_err("oversized must refuse");
        assert!(error.to_string().contains("holds only 1024 bytes"), "{error}");
    }

    #[test]
    fn a_plan_refuses_an_empty_image()
    {
        let error = Plan::new("disk", 1024, 0, Vec::new()).expect_err("empty must refuse");
        assert!(error.to_string().contains("the image is empty"), "{error}");
    }

    #[test]
    fn a_plan_shows_the_existing_contents_that_would_be_erased()
    {
        let plan = Plan::new("\\\\.\\PhysicalDrive3", 32 << 30, 512 << 20,
                             vec!["MBR: 1 partition, type 0x0c at sector 2048".into()])
            .unwrap();
        let text = plan.describe();
        assert!(text.contains("will be erased"), "{text}");
        assert!(text.contains("type 0x0c"), "{text}");
        assert!(text.contains("34359738368"), "{text}");
    }
}

/// Resolves the image this platform declares, then writes it to the target the
/// operator named.
///
/// The image never comes from an argument: it is whatever `hardware.boot_image`
/// names, resolved through the same path the emulator uses. A platform that
/// declares no boot image cannot be flashed, which is the correct outcome rather
/// than a tool that writes something arbitrary.
pub fn run(workspace: &crate::workspace::Workspace, target: &str, yes: bool) -> Result<()>
{
    let platform = mac_build::platform::current();
    let hardware = platform.hardware.as_ref().ok_or_else(|| {
                                                error(format!("platform '{}' declares no \
                                                               hardware section, so it names no \
                                                               boot image to flash",
                                                              platform.name))
                                            })?;
    let image_name = hardware.boot_image.as_deref().ok_or_else(|| {
                                                          error(format!("platform '{}' declares no \
                                                                         hardware.boot_image, so \
                                                                         there is nothing to flash",
                                                                        platform.name))
                                                      })?;
    let image_path = crate::boot::media(platform, &workspace.dist, image_name)?;
    if !image_path.exists()
    {
        return Err(error(format!("the image {} does not exist; build it first with 'cargo xtask \
                                  build'",
                                 image_path.display())));
    }
    let image = std::fs::read(&image_path)
        .map_err(|cause| error(format!("cannot read {}: {cause}", image_path.display())))?;

    let mut medium = device::open(target)?;
    let capacity = medium.capacity().unwrap_or(u64::MAX);
    let plan = Plan::new(target, capacity, image.len() as u64, existing_contents(target))?;

    println!("platform: {}", platform.name);
    println!("image:    {}", image_path.display());
    println!("{}", plan.describe());
    if !yes
    {
        return Err(error("nothing was written: this would erase the target. Re-run with --yes to \
                          write it"));
    }

    // An unbuffered raw device requires whole-sector writes, so the last sector
    // is zero-padded. The padding extends the write, never the image.
    let payload = if device::is_raw_device(target)
    {
        device::pad_to_sector(&image)
    }
    else
    {
        image.clone()
    };
    println!("writing {} bytes to {}...", payload.len(), target);
    flash(&mut medium, &payload)?;
    println!("verified: {} bytes written and read back identical", payload.len());
    Ok(())
}

/// Best-effort description of what the target currently holds, so the operator
/// can see what is about to be lost. A medium that cannot be read this way still
/// flashes; the report simply says the contents were not readable.
fn existing_contents(target: &str) -> Vec<String>
{
    if device::is_raw_device(target)
    {
        return vec!["raw device: existing partitions were not read before writing".into()];
    }
    match std::fs::metadata(target)
    {
        Ok(metadata) =>
        {
            if metadata.len() == 0
            {
                vec!["empty file".into()]
            }
            else
            {
                vec![format!("file of {} bytes", metadata.len())]
            }
        }
        Err(_) => vec!["contents not readable".into()]
    }
}