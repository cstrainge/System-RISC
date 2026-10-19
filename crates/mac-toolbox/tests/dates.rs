use mac_toolbox::Timestamp;
#[test]
fn dates_2026_survive_real_hfs_and_userspace_file_traps()
{
    mac_system::testing::with_disk(include_bytes!("data/files/dates-2026.hfs"), |services| {
        let system = unsafe { mac_toolbox::System::from_raw(services) }.unwrap();
        let files = system.files();
        let volume = files.volume_at(0).unwrap();
        assert_eq!(volume.created, 3_850_070_400);
        assert_eq!(volume.created_at().unix_seconds(), 1_767_225_600);
        assert_eq!(volume.modified_at().unix_seconds(), 1_792_326_896);
        for entry in [files.catalog(volume.id, 2).unwrap(),
                      files.lookup(volume.id, 2, "Folder").unwrap(),
                      files.lookup(volume.id, 2, "Folder/Café").unwrap()]
        {
            assert_eq!(entry.created_at().unix_seconds(), 1_767_225_600);
            assert_eq!(entry.modified_at().unix_seconds(), 1_792_326_896);
            assert_eq!(entry.backed_up_at().unix_seconds(), 1_798_761_599);
            assert!(entry.created > i32::MAX as u32);
            assert_eq!(entry.created_at().mac_seconds(), Some(entry.created));
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
