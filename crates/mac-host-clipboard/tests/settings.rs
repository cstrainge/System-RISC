use mac_host_clipboard::{Direction, MAX_SETTINGS_BYTES, SETTINGS_VERSION, Settings, SettingsError};

#[test]
fn version_one_defaults_and_all_typed_policies_round_trip_without_persistence_claims()
{
    let defaults = Settings::decode(br#"{"version":1}"#).unwrap();
    assert_eq!(defaults, Settings::default());
    assert!(defaults.enabled());
    assert_eq!(defaults.direction(), Direction::Both);
    assert_eq!(defaults.version(), SETTINGS_VERSION);
    for enabled in [false, true]
    {
        for direction in [Direction::Both, Direction::HostToGuest, Direction::GuestToHost]
        {
            let settings = Settings::new(enabled, direction);
            let mut bytes = [0x55; MAX_SETTINGS_BYTES];
            let used = settings.encode(&mut bytes).unwrap();
            assert_eq!(Settings::decode(&bytes[..used]).unwrap(), settings);
            assert!(bytes[used..].iter().all(|byte| *byte == 0x55));
            assert_eq!(settings.permits_import(), enabled && direction != Direction::GuestToHost);
            assert_eq!(settings.permits_export(), enabled && direction != Direction::HostToGuest);
        }
    }
    let escaped =
        br#"{ "\u0076ersion": 1, "enabled": true, "direction": "host\u002dto\u002dguest" } \n"#;
    // JSON permits escaped known keys/enum values, but only JSON whitespace
    // after the object. A literal backslash-n outside a string is invalid.
    assert!(Settings::decode(escaped).is_err());
    let escaped =
        br#"{ "\u0076ersion": 1, "enabled": true, "direction": "host\u002dto\u002dguest" }"#;
    assert_eq!(Settings::decode(escaped).unwrap().direction(), Direction::HostToGuest);
    assert_eq!(Settings::decode(b"{\"version\":1}\r\n\t ").unwrap(), defaults);
}

#[test]
fn malformed_unknown_duplicate_and_trailing_data_never_select_a_policy()
{
    for input in [&b""[..],
                  &b"{}"[..],
                  &b"[]"[..],
                  &br#"{"version":1,"unknown":true}"#[..],
                  &br#"{"version":1,"enabled":false,"enabled":true}"#[..],
                  &br#"{"version":1,"version":1}"#[..],
                  &br#"{"version":1,"enabled":"true"}"#[..],
                  &br#"{"version":1,"enabled":null}"#[..],
                  &br#"{"version":1,"enabled":1}"#[..],
                  &br#"{"version":1,"direction":"Both"}"#[..],
                  &br#"{"version":1,"direction":"both\q"}"#[..],
                  &br#"{"version":1,"direction":"host-to-guest","direction":"both"}"#[..],
                  &br#"{"version":1.0}"#[..],
                  &br#"{"version":01}"#[..],
                  &br#"{"version":-1}"#[..],
                  &br#"{"version":4294967296}"#[..],
                  &br#"{"version":1}{}"#[..],
                  &br#"{"version":1}true"#[..],
                  &b"{\"version\":1}\0"[..],
                  &b"{\"version\":1}\x0b"[..]]
    {
        let error = Settings::decode(input).unwrap_err();
        assert!(matches!(error, SettingsError::Json(_)), "{input:?}: {error}");
        let cause = error.to_string();
        for expected in
            ["preferences", "version 1", "unknown/duplicate", "retain the active policy"]
        {
            assert!(cause.contains(expected), "{cause}");
        }
    }
    for version in [0, 2, u32::MAX]
    {
        let input = format!("{{\"version\":{version}}}");
        let error = Settings::decode(input.as_bytes()).unwrap_err();
        assert!(matches!(error,SettingsError::Version { observed } if observed==version));
        let cause = error.to_string();
        for expected in ["unsupported version", "requires version 1", "recreate"]
        {
            assert!(cause.contains(expected), "{cause}");
        }
    }
}

#[test]
fn input_and_output_admission_are_bounded_and_short_output_preserves_caller_storage()
{
    let minimal = br#"{"version":1}"#;
    let mut maximum = [b' '; MAX_SETTINGS_BYTES];
    maximum[..minimal.len()].copy_from_slice(minimal);
    assert_eq!(Settings::decode(&maximum).unwrap(), Settings::default());
    let over = [b' '; MAX_SETTINGS_BYTES + 1];
    let error = Settings::decode(&over).unwrap_err();
    assert!(matches!(error,SettingsError::Size { bytes,maximum } if bytes==MAX_SETTINGS_BYTES+1&&maximum==MAX_SETTINGS_BYTES));
    assert!(error.to_string().contains("smaller version 1 document"));
    let mut short = [0x55; 8];
    let error = Settings::default().encode(&mut short).unwrap_err();
    assert!(matches!(error,SettingsError::Output { required,available:8 } if required>8));
    assert_eq!(short, [0x55; 8]);
    assert!(error.to_string().contains("retry with a larger buffer"));
    let mut complete = [0; MAX_SETTINGS_BYTES];
    let used = Settings::default().encode(&mut complete).unwrap();
    assert_eq!(Settings::decode(&complete[..used]).unwrap(), Settings::default());
}
