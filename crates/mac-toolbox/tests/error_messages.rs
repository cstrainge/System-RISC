//! Error identity and explanations are part of the public SDK contract.
use mac_toolbox::{Error, files::ArchiveError};

#[test]
fn archive_causes_and_payloads_survive_the_public_error_boundary()
{
    for (cause, explanation) in
        [(ArchiveError::Truncated, "required header or fork bytes are missing"),
         (ArchiveError::Signature, "StuffIt archive signature"),
         (ArchiveError::HeaderChecksum, "header checksum does not match"),
         (ArchiveError::ForkChecksum, "extracted bytes failed verification"),
         (ArchiveError::Malformed, "invalid lengths, hierarchy or compressed data"),
         (ArchiveError::Limit, "catalog, nesting or decoder limits"),
         (ArchiveError::Encrypted, "password-protected extraction is not supported")]
    {
        let error = Error::from_status(cause.status());
        assert_eq!(error, Error::Archive(cause));
        assert_eq!(error.status(), cause.status());
        assert!(error.to_string().contains(explanation), "{error}");
    }
    for byte in 0..=255
    {
        for (cause, what) in [(ArchiveError::Version(byte), "archive version"),
                              (ArchiveError::Method(byte), "compression method"),
                              (ArchiveError::EntryVersion(byte), "entry header version")]
        {
            let error = Error::from_status(cause.status());
            assert_eq!(error, Error::Archive(cause));
            assert_eq!(error.status(), cause.status());
            let text = error.to_string();
            assert!(text.contains(&format!("{what} {byte}")), "{text}");
            assert!(text.contains("installed driver"), "{text}");
        }
    }
    for other in [i32::MIN, -65_543, -65_791, -66_560, i32::MAX]
    {
        assert_eq!(Error::from_status(other), Error::Unknown(other));
        assert!(Error::from_status(other).to_string().contains(&other.to_string()));
    }
}
