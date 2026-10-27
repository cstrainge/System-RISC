use mac_byte_channel::{State, Status, accepted};
#[test]
fn status_checks_generation_state_status_and_exact_wire_length()
{
    let status = Status { state:       State::Open,
                          last_status: 0,
                          generation:  17 };
    assert_eq!(Status::decode(&status.encode()), Ok(status));
    assert!(Status::decode(&[0; 15]).is_err());
    assert!(Status::decode(&Status { generation: 0,
                                     ..status }.encode()).is_err());
    assert!(Status::decode(&Status { last_status: 1,
                                     ..status }.encode()).is_err());
    let mut bytes = status.encode();
    bytes[0] = 5;
    assert!(Status::decode(&bytes).is_err());
    assert_eq!(accepted(&3u32.to_le_bytes(), 5), Ok(3));
    assert_eq!(accepted(&0u32.to_le_bytes(), 5), Ok(0));
    assert!(accepted(&6u32.to_le_bytes(), 5).is_err());
    assert!(accepted(&[0; 3], 5).is_err());
}
