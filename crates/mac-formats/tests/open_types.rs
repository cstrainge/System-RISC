use mac_formats::{Error,
                  open_types::{self, OpenTypes}};

#[test]
fn classic_translation_manager_declarations_round_trip_and_distinguish_types()
{
    // Apple develop18, p87 Rez layout; PICT added to its TEXT example.
    let original = b"TSTR\0\0\0\x02TEXTPICT";
    let parsed = OpenTypes::decode(original).unwrap();
    assert_eq!(parsed.signature(), *b"TSTR");
    assert!(parsed.accepts(*b"TEXT"));
    assert!(parsed.accepts(*b"PICT"));
    assert!(!parsed.accepts(*b"APPL"));
    let mut encoded = [0xa5; 20];
    assert_eq!(open_types::encode(parsed.signature(), parsed.types(), &mut encoded).unwrap(), 16);
    assert_eq!(&encoded[..16], original);
    assert_eq!(&encoded[16..], &[0xa5; 4]);
    let empty = OpenTypes::decode(b"TSTR\0\0\0\0").unwrap();
    assert!(empty.types().is_empty());
    assert!(!empty.accepts(*b"TEXT"), "an empty list must not imply all types");
}

#[test]
fn malformed_open_declarations_preserve_specific_diagnostics()
{
    for (bytes, message) in
        [(b"TSTR\0\0".as_slice(), "truncated open-types header"),
         (b"TSTR\0\x01\0\0", "open-types reserved word must be zero"),
         (b"TSTR\0\0\0\x02TEXT", "open-types count does not match resource length"),
         (b"TSTR\0\0\0\0TEXT", "open-types count does not match resource length")]
    {
        let error = OpenTypes::decode(bytes).unwrap_err();
        assert_eq!(error, Error::Malformed(message));
        assert_eq!(error.to_string(), message);
    }
    let mut short = [0xa5; 8];
    assert!(matches!(open_types::encode(*b"TSTR", &[*b"TEXT"], &mut short),
                     Err(Error::Malformed("open-types output buffer is too small"))));
    assert_eq!(short, [0xa5; 8]);
}
