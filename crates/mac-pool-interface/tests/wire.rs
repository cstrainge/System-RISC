//! The wire pair must agree: what `encode` produces is what `decode` accepts,
//! and what either names as `BYTES` is the length that travels.
use mac_pool_interface::{Lease, MAX_REPLY, Request_};

#[test]
fn a_lease_request_round_trips_at_its_declared_length()
{
    let request = Request_ { key:          *b"brcm,dma\0\0\0\0\0\0\0\0",
                              requirement: Request_::ANY_RESOURCE };
    let bytes = request.encode();
    assert_eq!(bytes.len(), Request_::BYTES);
    assert_eq!(Request_::decode(&bytes), Some(request));
}

#[test]
fn a_lease_reply_round_trips_at_its_declared_length()
{
    let lease = Lease { handle:   0x0102_0304_0506_0708,
                        resource: 14 };
    let bytes = lease.encode();
    assert_eq!(bytes.len(), Lease::BYTES);
    assert_eq!(Lease::decode(&bytes), Some(lease));
}

/// The defect this pins: an encoder that padded its record to a buffer larger
/// than `BYTES` produced bytes its own reader refuses.
#[test]
fn a_padded_record_is_refused_rather_than_silently_shortened()
{
    let request = Request_ { key:          [0x11; 16],
                              requirement: 3 };
    let mut padded = [0; 24];
    padded[..Request_::BYTES].copy_from_slice(&request.encode());
    assert_eq!(Request_::decode(&padded), None);

    let lease = Lease { handle: 7,
                        resource: 2 };
    let mut padded = [0; 16];
    padded[..Lease::BYTES].copy_from_slice(&lease.encode());
    assert_eq!(Lease::decode(&padded), None);
}

#[test]
fn the_reply_buffer_holds_every_reply_the_class_returns()
{
    assert!(MAX_REPLY >= Lease::BYTES);
}
