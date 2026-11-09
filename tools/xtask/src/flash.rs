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
    /// What a flash would do, for a target whose capacity may not be known.
    ///
    /// A target whose size could not be read is refused rather than treated as
    /// unbounded. The previous code passed `unwrap_or(u64::MAX)` here, which made
    /// the oversize branch below unreachable: every image fits in `u64::MAX`, so the
    /// check that exists to protect the one unrecoverable operation in this tool
    /// could never fire. Writing past the end of a card truncates the image, and the
    /// operator's evidence of it is on the card.
    pub fn for_target(target: &str, capacity: Option<u64>, image_bytes: u64,
                      existing: Vec<String>)
                      -> Result<Self>
    {
        let capacity = capacity.ok_or_else(|| {
                                           error(format!("cannot determine how many bytes {target} \
                                                          holds, so an image that does not fit \
                                                          cannot be refused; nothing was written"))
                                       })?;
        Self::new(target, capacity, image_bytes, existing)
    }

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
    // Refuse a platform with no card before anything else, so the message names
    // what that platform actually boots from rather than a missing image.
    removable_media(platform)?;
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
    // Refuse an image that no longer matches the artifacts it claims to contain.
    // Writing a stale image to a card is the one mistake here that cannot be
    // corrected afterwards, because the evidence of it is on the card.
    crate::disk::verify(workspace)?;
    let image = std::fs::read(&image_path)
        .map_err(|cause| error(format!("cannot read {}: {cause}", image_path.display())))?;

    let mut medium = device::open(target)?;
    let capacity = medium.capacity();
    let plan = Plan::for_target(target, capacity, image.len() as u64, existing_contents(target))?;

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
/// Whether this platform boots from removable media, which is the only kind of
/// platform this tool may write.
///
/// The gate is the record, not a list of platform names: a machine whose boot
/// medium is an SD card has a card to flash, and an emulator that attaches a
/// drive to a virtual machine does not. A tool that can be pointed at a virt
/// platform is a tool someone will point at a virt platform.
pub fn removable_media(platform: &mac_build::platform::Platform) -> Result<()>
{
    use mac_build::platform::hardware::Medium;
    let hardware = platform.hardware.as_ref();
    let removable = hardware.map(|hardware| matches!(hardware.boot.medium, Medium::Sd { .. }))
                            .unwrap_or(false);
    if removable
    {
        return Ok(());
    }
    let boots_from = match hardware
    {
        Some(hardware) => format!("its boot medium is {:?}", hardware.boot.medium),
        None => "it declares no hardware boot medium".to_string()
    };
    Err(error(format!("platform '{}' is not a removable-media machine: {boots_from}. Flashing \
                       writes a physical card, so it applies only to a platform whose record \
                       declares an SD boot medium. This platform boots in an emulator, which \
                       attaches its image directly.",
                      platform.name)))
}

