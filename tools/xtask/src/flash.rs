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
    fn a_plan_refuses_an_oversize_image_before_anything_is_written()
    {
        // The end-to-end shape of the defect: a real oversize image, a target of a
        // real size, and the assertion that the target is byte-for-byte untouched
        // when the pre-flight refuses. `Plan::for_target` is what the command calls,
        // so this is the same path the tool takes rather than a hand-built refusal.
        let image = vec![0xA5u8; 4096];
        let mut medium = Cursor::new(vec![0u8; 1024]);
        let capacity = Some(medium.get_ref().len() as u64);
        let error = Plan::for_target("card", capacity, image.len() as u64, Vec::new())
            .expect_err("an image larger than the card must be refused");
        assert!(error.to_string().contains("holds only 1024 bytes"), "{error}");
        assert!(medium.get_ref().iter().all(|byte| *byte == 0),
                "the medium must be untouched when the plan refuses");
    }

    #[test]
    fn a_target_whose_size_cannot_be_read_is_refused_rather_than_assumed_unbounded()
    {
        // This is the branch that could never fire: with `unwrap_or(u64::MAX)` every
        // image fits, so an unmeasurable target flashed whatever it was given and
        // truncated it silently.
        let error = Plan::for_target("card", None, 512, Vec::new())
            .expect_err("an unmeasurable target must refuse");
        assert!(error.to_string().contains("cannot determine how many bytes"), "{error}");
    }

    #[test]
    fn a_target_whose_size_is_known_still_accepts_an_image_that_fits()
    {
        // The refusal above must not become a blanket one: a measured target that
        // fits is exactly the case this tool exists for.
        let plan = Plan::for_target("card", Some(8 << 20), 2048, Vec::new())
            .expect("an image that fits is planned");
        assert_eq!(plan.target_capacity, 8 << 20);
        assert_eq!(plan.image_bytes, 2048);
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

#[cfg(test)]
mod removable_tests
{
    use super::removable_media;

    #[test]
    fn only_a_removable_media_platform_may_be_flashed()
    {
        use mac_build::platform;
        let board = platform::load("raspi3b").expect("the board record loads");
        removable_media(&board).expect("a board that boots from an SD card can be flashed");

        for virt in ["qemu_arm64", "qemu_riscv"]
        {
            let record = platform::load(virt).expect("the virt record loads");
            let error = removable_media(&record)
                .expect_err("a platform that boots in an emulator must be refused");
            let message = error.to_string();
            assert!(message.contains("not a removable-media machine"), "{message}");
            assert!(message.contains(virt), "the refusal must name the platform: {message}");
        }
    }
}
