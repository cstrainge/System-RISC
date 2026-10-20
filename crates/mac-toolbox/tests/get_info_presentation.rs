//! Finder presentation regressions using dates returned by the public file SDK.
#![cfg(not(target_arch = "riscv64"))]
#[path = "../../../finder/src/get_info/format.rs"]
mod presentation;
use mac_toolbox::Timestamp;
use presentation::Calendar;

#[test]
fn finder_formats_real_unsigned_2026_catalog_dates_without_signed_time_t()
{
    mac_system::testing::with_disk(include_bytes!("data/files/dates-2026.hfs"), |services| {
        let system = unsafe { mac_toolbox::System::from_raw(services) }.unwrap();
        let file = system.files()
                         .lookup(system.files().volume_at(0).unwrap().id, 2, "Folder/Café")
                         .unwrap();
        assert!(file.created_at().mac_seconds().unwrap() > i32::MAX as u32);
        assert_eq!(presentation::calendar(file.created_at()),
                   Calendar { year:    2026,
                              month:   1,
                              day:     1,
                              weekday: 4,
                              hour:    0,
                              minute:  0 });
        assert_eq!(presentation::calendar(file.modified_at()),
                   Calendar { year:    2026,
                              month:   10,
                              day:     18,
                              weekday: 0,
                              hour:    12,
                              minute:  34 });
    });
    // Original controlled Get Info fixture, independently read from its catalog.
    assert_eq!(presentation::calendar(Timestamp::from_mac_seconds(3_875_209_985)),
               Calendar { year:    2026,
                          month:   10,
                          day:     18,
                          weekday: 0,
                          hour:    23,
                          minute:  13 });
    assert_eq!(presentation::calendar(Timestamp::from_unix_seconds(2_147_483_648)),
               Calendar { year:    2038,
                          month:   1,
                          day:     19,
                          weekday: 2,
                          hour:    3,
                          minute:  14 });
}

#[test]
fn finder_calendar_handles_pre_unix_and_gregorian_leap_boundaries()
{
    for (seconds, expected) in [(-1,
                                 Calendar { year:    1969,
                                            month:   12,
                                            day:     31,
                                            weekday: 3,
                                            hour:    23,
                                            minute:  59 }),
                                (951_782_400,
                                 Calendar { year:    2000,
                                            month:   2,
                                            day:     29,
                                            weekday: 2,
                                            hour:    0,
                                            minute:  0 }),
                                (4_107_542_400,
                                 Calendar { year:    2100,
                                            month:   3,
                                            day:     1,
                                            weekday: 1,
                                            hour:    0,
                                            minute:  0 }),
                                (Timestamp::MAC_TO_UNIX_SECONDS.saturating_neg(),
                                 Calendar { year:    1904,
                                            month:   1,
                                            day:     1,
                                            weekday: 5,
                                            hour:    0,
                                            minute:  0 })]
    {
        assert_eq!(presentation::calendar(Timestamp::from_unix_seconds(seconds)), expected);
    }
}

#[test]
fn original_get_info_used_bytes_and_counts_group_without_overflow()
{
    for (value, expected) in [(0, "0"),
                              (36, "36"),
                              (999, "999"),
                              (1000, "1,000"),
                              (6912, "6,912"),
                              (16384, "16,384"),
                              (515019, "515,019"),
                              (u64::MAX, "18,446,744,073,709,551,615")]
    {
        assert_eq!(presentation::grouped(value).as_str(), expected);
    }
}
