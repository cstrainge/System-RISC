//! The Finder a bootable installation must carry beside System.
//!
//! A blessed System Folder holds two native executables: the System the
//! bootstrap loads, and the Finder System starts when the desktop comes up.
//! Both are admitted by the same rules, and the bootstrap is where that is
//! decided. Entering System with a Finder that cannot run would turn a volume
//! the ROM should have refused into a boot that dies after the handoff - later,
//! and in a log belonging to the other side of the jump - so every bootstrap
//! asks this module the same question about the same folder instead of keeping
//! its own copy of the checks.
//!
//! Admission is not execution. The code and relocation records are decoded to
//! prove the kernel can enter the image; nothing read here is run or retained.

use mac_filesystem::{BlockDevice, Volume};
use mac_formats::{native::{Descriptor, Role, image},
                  resource};

/// The window a platform admits an application image into.
///
/// The addresses and the granule come from the platform record rather than from
/// this module. An application image is placed where the kernel will map it, so
/// a bootstrap that assumed one platform's window would refuse a valid Finder
/// on the machine that states another.
pub struct Application
{
    /// The virtual address an application image is admitted at.
    pub image_base: u64,
    /// The exclusive end of the address range an application may occupy.
    pub end:        u64,
    /// The platform's translation granule.
    pub page_bytes: u64
}

/// Reads the blessed folder's `Finder` and admits it against `application`.
///
/// `budget` bounds the streaming read, and the caller supplies it because the
/// heap that pays for the retained payload is the caller's. The payload is
/// dropped before this returns, so a bootstrap checks the companion before it
/// retains the larger System image.
pub fn admit<D: BlockDevice>(volume: &mut Volume<D>, folder: u32,
                             budget: resource::stream::Budget, application: Application)
                             -> Result<(), mac_formats::Error>
{
    let file = volume.find_file(folder, b"Finder")?;
    if file.file_type != *b"FNDR"
    {
        return Err(mac_formats::Error::Malformed("startup Finder does not have type FNDR"));
    }
    let mut fork = file.resources;
    let finder =
        resource::stream::read_selected(fork.length,
                                        budget,
                                        |kind, id| {
                                            id == 0 && matches!(&kind, b"NABI" | b"RV64" | b"RLOC")
                                        },
                                        |offset, output| {
                                            let count = volume.read(&mut fork, offset, output)?;
                                            if count == output.len()
                                            {
                                                Ok(())
                                            }
                                            else
                                            {
                                                Err(mac_formats::Error::Malformed("short Finder \
                                                                                   resource read"))
                                            }
                                        })?;
    let get = |kind| {
        finder.iter()
              .find(|row| row.kind == kind)
              .ok_or(mac_formats::Error::Malformed("startup Finder code metadata is missing"))
    };
    let abi = Descriptor::decode(&get(*b"NABI")?.data)?;
    if abi.role != Role::Application
       || abi.abi_version != mac_abi::ABI_VERSION
       || abi.load_base != application.image_base
       || abi.load_limit > application.end
       || !abi.load_limit.is_multiple_of(application.page_bytes)
    {
        return Err(mac_formats::Error::Malformed("startup Finder is incompatible; rebuild \
                                                  System disk"));
    }
    // The plan is built and dropped: its value is the check that the code and
    // its relocation records describe an image the kernel can enter, not a copy
    // this bootstrap intends to run.
    image::LoadPlan::new(&get(*b"RV64")?.data, &abi, Some(&get(*b"RLOC")?.data), abi.load_base)?;
    Ok(())
}
