#![allow(dead_code)]
//! Native 64-bit date values and checked unsigned legacy conversion.
use mac_toolbox::Timestamp;

use super::{Failure, require};
pub fn values() -> Result<(), Failure>
{
    require(Timestamp::from_mac_seconds(0).unix_seconds() == -2_082_844_800, "Mac epoch")?;
    require(Timestamp::from_unix_seconds(0).mac_seconds() == Some(2_082_844_800), "Unix epoch")?;
    require(Timestamp::from_mac_seconds(u32::MAX).unix_seconds() == 2_212_122_495,
            "last classic second")?;
    for seconds in [-2_082_844_801, 2_212_122_496, i64::MAX, i64::MIN]
    {
        require(Timestamp::from_unix_seconds(seconds).mac_seconds().is_none(),
                "classic date overflow rejected")?;
    }
    for (unix, mac) in [(1_767_225_600, 3_850_070_400),
                        (1_792_326_896, 3_875_171_696),
                        (1_798_761_599, 3_881_606_399),
                        (2_147_483_648, 4_230_328_448)]
    {
        let native = Timestamp::from_unix_seconds(unix);
        require(native.mac_seconds() == Some(mac) && Timestamp::from_mac_seconds(mac) == native,
                "2026 and signed-2038 roundtrip")?;
    }
    Ok(())
}

/// The graphical harness mounts the synthetic2026 HFS image as a second disk.
/// All assertions use the public userspace File Manager, including real ECALLs.
pub fn filesystem(system: &mac_toolbox::System<'_>) -> Result<(), Failure>
{
    let files = system.files();
    let mut fixture = None;
    for i in 0..files.volume_count()?
    {
        let volume = files.volume_at(i)?;
        if volume.name() == "Test"
        {
            fixture = Some(volume);
            break;
        }
    }
    let volume = fixture.ok_or(Failure::Mismatch("missing2026HFSfixture"))?;
    require(volume.created_at().unix_seconds() == 1_767_225_600, "2026 volume creation")?;
    require(volume.modified_at().unix_seconds() == 1_792_326_896, "2026 volume modification")?;
    for entry in [files.catalog(volume.id, 2)?,
                  files.lookup(volume.id, 2, "Folder")?,
                  files.lookup(volume.id, 2, "Folder/Café")?]
    {
        require(entry.created_at().unix_seconds() == 1_767_225_600, "2026 catalog creation")?;
        require(entry.modified_at().unix_seconds() == 1_792_326_896, "2026 catalog modification")?;
        require(entry.backed_up_at().unix_seconds() == 1_798_761_599, "2026 catalog backup")?;
        require(entry.created_at().mac_microseconds() == Some(entry.created)
                && entry.created_at()
                        .mac_seconds()
                        .is_some_and(|seconds| seconds > i32::MAX as u32),
                "unsigned classic metadata")?;
    }
    struct Path
    {
        bytes:  [u8; 128],
        length: usize
    }
    impl core::fmt::Write for Path
    {
        fn write_str(&mut self, text: &str) -> core::fmt::Result
        {
            let end = self.length.checked_add(text.len()).ok_or(core::fmt::Error)?;
            let output = self.bytes.get_mut(self.length..end).ok_or(core::fmt::Error)?;
            output.copy_from_slice(text.as_bytes());
            self.length = end;
            Ok(())
        }
    }
    let mut path = Path { bytes:  [0; 128],
                          length: 0 };
    files.path(volume.id, 19, &mut path)?;
    require(&path.bytes[..path.length] == "/Folder/Café".as_bytes(), "2026 fixture UTF8 path")?;
    Ok(())
}
