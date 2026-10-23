use mac_formats::{Error, authority::*, resource::Resource};

fn resource(kind: [u8; 4], id: i16) -> Resource
{
    Resource { kind,
               id,
               name: Some(b"Code".to_vec()),
               attributes: 0,
               data: vec![1, 2, 3] }
}
#[test]
fn policy_binds_every_resource_field_and_is_independent_of_fork_layout()
{
    let resources = vec![resource(*b"RV64", 0), resource(*b"STR ", 128)];
    let identity = image_digest(&resources).unwrap();
    let policy = encode(&[(identity, INSPECT_DOMAINS)]).unwrap();
    assert_eq!(granted(&policy, identity).unwrap(), INSPECT_DOMAINS);
    let mut reordered = resources.clone();
    reordered.reverse();
    assert_eq!(image_digest(&reordered).unwrap(), identity);
    for field in 0..6
    {
        let mut changed = resources.clone();
        match field
        {
            0 => changed[0].kind = *b"CODE",
            1 => changed[0].id += 1,
            2 => changed[0].attributes ^= 1,
            3 => changed[0].name = None,
            4 => changed[0].name = Some(b"Different".to_vec()),
            _ => changed[0].data[0] ^= 1
        }
        let digest = image_digest(&changed).unwrap();
        assert_ne!(digest, identity);
        assert_eq!(granted(&policy, digest).unwrap(),
                   0,
                   "tampering field {field} must remove authority");
    }
    let duplicate = image_digest(&[resources[0].clone(), resources[0].clone()]).unwrap_err();
    assert!(matches!(duplicate, Error::Malformed(_)));
    assert!(duplicate.to_string().contains("duplicate resource"));
}

#[test]
fn malformed_policy_never_grants_from_a_valid_prefix()
{
    let identity = [7; 32];
    let policy = encode(&[(identity, INSPECT_DOMAINS), ([9; 32], INSPECT_DOMAINS)]).unwrap();
    for length in 0..policy.len()
    {
        let error = granted(&policy[..length], identity).unwrap_err();
        assert!(matches!(error, Error::Malformed(_)));
        assert!(error.to_string().contains("application authority policy"), "{error}");
    }
    let mut unknown = policy.clone();
    *unknown.last_mut().unwrap() = 2;
    let error = granted(&unknown, identity).unwrap_err();
    assert!(error.to_string().contains("unknown or empty"));
    let mut trailing = policy.clone();
    trailing.push(0);
    assert!(granted(&trailing, identity).unwrap_err().to_string().contains("trailing"));
    assert!(encode(&[(identity, 0)]).unwrap_err().to_string().contains("empty"));
}
