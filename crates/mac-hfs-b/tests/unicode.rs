use mac_hfs_b::names;
#[test]
fn normalization_and_caseless_vectors()
{
    assert_eq!(names::nfc("\u{1100}\u{1161}\u{11a8}").unwrap(), "각");
    for (a, b) in
        [("café", "CAFE\u{301}"), ("Straße", "STRASSE"), ("ﬁ", "fi"), ("I", "i"), ("İ", "i\u{307}")]
    {
        assert_eq!(names::comparison(a), names::comparison(b));
    }
    assert_ne!(names::comparison("I"), names::comparison("ı"));
    assert_ne!(names::comparison("①"), names::comparison("1"));
}
#[test]
#[ignore = "set HFS_B_NORMALIZATION_TEST to the pinned official Unicode 17.0.0 corpus"]
fn official_unicode_normalization_conformance()
{
    let path = std::env::var("HFS_B_NORMALIZATION_TEST").unwrap();
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.contains("NormalizationTest-17.0.0"));
    let mut count = 0;
    for line in text.lines()
    {
        let line = line.split('#').next().unwrap().trim();
        if line.is_empty() || line.starts_with('@')
        {
            continue;
        }
        let c: Vec<String> = line.split(';')
                                 .take(5)
                                 .map(|part| {
                                     part.split_whitespace()
            .map(|n|char::from_u32(u32::from_str_radix(n,16).unwrap()).unwrap()).collect()
                                 })
                                 .collect();
        for i in 0..3
        {
            assert_eq!(names::nfc(&c[i]).unwrap(), c[1], "row {count} input {i}");
        }
        for i in 3..5
        {
            assert_eq!(names::nfc(&c[i]).unwrap(), c[3], "row {count} input {i}");
        }
        count += 1;
    }
    assert!(count > 19_000);
    eprintln!("Unicode 17.0.0 NFC conformance: {count} rows passed");
}
