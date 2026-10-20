use mac_toolbox::Timestamp;

#[test]
#[ignore = "requires the current cargo xtask image and its package metadata"]
fn built_disk_dates_match_package_metadata_and_the_real_image_creation_clock()
{
    use std::{fs, path::PathBuf};
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let dist = mac_build::paths::dist_dir(&root);
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(dist.join("disk.json")).unwrap()).unwrap();
    let start = report["image_started_unix"].as_i64().unwrap();
    let finish = report["image_finished_unix"].as_i64().unwrap();
    assert!(finish >= start);
    let within_build = |date: Timestamp| {
        assert!((start..=finish).contains(&date.unix_seconds()),
                "disk metadata{} is outside its real creation interval{start}..={finish}",
                date.unix_seconds());
    };
    let (_, activity) = mac_system::testing::with_file_disk(&dist.join("system7.hfsb"), true, Default::default(), |services| {
        let system = unsafe { mac_toolbox::System::from_raw(services) }.unwrap();
        let files = system.files();
        let volume = files.volume_at(0).unwrap();
        assert_eq!(volume.created, report["created"].as_u64().unwrap());
        assert_eq!(volume.modified, report["modified"].as_u64().unwrap());
        within_build(volume.created_at());
        within_build(volume.modified_at());
        for path in ["", "System Folder", "Applications", "Documents", "System Folder/Preferences"] {
            let folder = if path.is_empty() { files.catalog(volume.id, 2).unwrap() }
                         else { files.lookup(volume.id, 2, path).unwrap() };
            within_build(folder.created_at());
            within_build(folder.modified_at());
        }
        for file in report["files"].as_array().unwrap() {
            let path = file["path"].as_str().unwrap();
            let name = path.rsplit('/').next().unwrap();
            let catalog = files.lookup(volume.id, 2, path).unwrap();
            let package = mac_formats::macbinary::decode(&fs::read(dist.join(format!("{name}.bin"))).unwrap()).unwrap();
            let metadata: serde_json::Value = serde_json::from_slice(&fs::read(dist.join(format!("{name}.json"))).unwrap()).unwrap();
            assert_eq!(catalog.created, u64::from(package.created)*1_000_000, "{path}: creation changed on import");
            assert_eq!(catalog.modified, u64::from(package.modified)*1_000_000, "{path}: modification changed on import");
            assert_eq!(catalog.created/1_000_000, metadata["created"].as_u64().unwrap());
            assert_eq!(catalog.modified/1_000_000, metadata["modified"].as_u64().unwrap());
            let packaged_at = metadata["packaged_at_unix"].as_i64().unwrap();
            assert!(packaged_at <= start, "{path}: packaged after image creation started");
            let expected = metadata["source_date_epoch"].as_i64().unwrap_or(packaged_at);
            assert_eq!(catalog.created_at().unix_seconds(), expected, "{path}: creation is not its declared build time");
            assert_eq!(catalog.modified_at().unix_seconds(), expected);
        }
    }).unwrap();
    assert_eq!(activity.writes, 0, "date inspection modified the built disk");
}

#[test]
fn dates_2026_survive_real_hfs_and_userspace_file_traps()
{
    mac_system::testing::with_disk(include_bytes!("data/files/dates-2026.hfs"), |services| {
        let system = unsafe { mac_toolbox::System::from_raw(services) }.unwrap();
        let files = system.files();
        let volume = files.volume_at(0).unwrap();
        assert_eq!(volume.created, 3_850_070_400_000_000);
        assert_eq!(volume.created_at().unix_seconds(), 1_767_225_600);
        assert_eq!(volume.modified_at().unix_seconds(), 1_792_326_896);
        for entry in [files.catalog(volume.id, 2).unwrap(),
                      files.lookup(volume.id, 2, "Folder").unwrap(),
                      files.lookup(volume.id, 2, "Folder/Café").unwrap()]
        {
            assert_eq!(entry.created_at().unix_seconds(), 1_767_225_600);
            assert_eq!(entry.modified_at().unix_seconds(), 1_792_326_896);
            assert_eq!(entry.backed_up_at().unix_seconds(), 1_798_761_599);
            assert!(entry.created_at().mac_seconds().unwrap() > i32::MAX as u32);
            assert_eq!(entry.created_at().mac_microseconds(), Some(entry.created));
        }
        let mut path = String::new();
        files.path(volume.id, 19, &mut path).unwrap();
        assert_eq!(path, "/Folder/Café");
    })
}
#[test]
fn classic_date_limits_fail_explicitly_while_native_dates_continue()
{
    assert_eq!(Timestamp::from_mac_seconds(0).unix_seconds(), -2_082_844_800);
    assert_eq!(Timestamp::from_unix_seconds(0).mac_seconds(), Some(2_082_844_800));
    assert_eq!(Timestamp::from_mac_seconds(u32::MAX).unix_seconds(), 2_212_122_495);
    for seconds in [-2_082_844_801, 2_212_122_496, i64::MAX, i64::MIN]
    {
        assert_eq!(Timestamp::from_unix_seconds(seconds).mac_seconds(), None);
    }
    for seconds in [1_767_225_600, 1_792_326_896, 1_798_761_599, 2_147_483_648]
    {
        let native = Timestamp::from_unix_seconds(seconds);
        assert_eq!(Timestamp::from_mac_seconds(native.mac_seconds().unwrap()), native);
    }
}
