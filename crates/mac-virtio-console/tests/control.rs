use mac_virtio_console::{Diagnostic, VDAGENT_PORT,
                         control::{Control, Event}};
fn record(id: u32, event: u16, value: u16, body: &[u8]) -> Vec<u8>
{
    let mut bytes = Vec::from(id.to_le_bytes());
    bytes.extend(event.to_le_bytes());
    bytes.extend(value.to_le_bytes());
    bytes.extend(body);
    bytes
}
#[test]
fn qemu_named_profile_requires_multiport_and_never_publishes_port_zero_queues()
{
    use mac_virtio_console::{QUEUE_INDICES, validate_profile};
    let features = (1u64 << 1) | (1u64 << 32);
    assert_eq!(validate_profile(features, 2), Ok(()));
    for (offered, slots) in [(features, 1), (features, 3), (1 << 32, 2), (1 << 1, 2)]
    {
        assert_eq!(validate_profile(offered, slots), Err(Diagnostic::Profile));
    }
    assert_eq!(QUEUE_INDICES, [4, 5, 2, 3]);
    for (index, queue) in QUEUE_INDICES.iter().enumerate()
    {
        assert!(*queue >= 2);
        assert!(!QUEUE_INDICES[..index].contains(queue));
    }
}
fn added() -> Control
{
    let mut control = Control::new();
    assert_eq!(control.next(), Some(record(1, 0, 1, &[]).try_into().unwrap()));
    control.published();
    assert_eq!(control.ingest(VDAGENT_PORT, &record(1, 1, 1, &[])), Ok(Event::Progress));
    assert_eq!(control.next(), Some(record(1, 3, 1, &[]).try_into().unwrap()));
    control.published();
    control
}
#[test]
fn identity_and_both_open_notifications_are_required_before_bytes()
{
    for trailing_nul in [false, true]
    {
        let mut control = added();
        control.ingest(VDAGENT_PORT, &record(1, 6, 1, &[])).unwrap();
        assert!(!control.connected());
        let mut name = VDAGENT_PORT.as_bytes().to_vec();
        if trailing_nul
        {
            name.push(0);
        }
        assert_eq!(control.ingest(VDAGENT_PORT, &record(1, 7, 1, &name)), Ok(Event::Progress));
        assert!(control.identified());
        assert!(!control.connected());
        let open = control.next();
        assert_eq!(open, Some(record(1, 6, 1, &[]).try_into().unwrap()));
        assert_eq!(control.next(), open, "backpressure must preserve the unsent guest-open record");
        control.published();
        assert!(control.connected());
        assert_eq!(control.ingest(VDAGENT_PORT, &record(1, 6, 0, &[])), Ok(Event::Disconnected));
    }
}
#[test]
fn wrong_port_name_is_a_typed_refusal_and_never_opens_the_port()
{
    let mut control = added();
    assert_eq!(control.ingest(VDAGENT_PORT,
                              &record(1, 7, 1, b"org.system-risc.host-filesystem\0")),
               Ok(Event::RejectName));
    assert!(!control.identified());
    assert!(!control.connected());
    assert!(control.next().is_none());
    assert!(Diagnostic::NameMismatch.message().contains("required virtio-console port name"));
    assert!(Diagnostic::Profile.message().contains("max_ports=2"));
}
#[test]
fn malformed_foreign_duplicate_and_out_of_order_controls_fail_informatively()
{
    let mut foreign = added();
    assert_eq!(foreign.ingest(VDAGENT_PORT, &record(0, 1, 1, &[])), Err(Diagnostic::Profile));
    for bytes in
        [record(1, 99, 1, &[]), record(1, 6, 2, &[]), record(1, 1, 1, b"extra"), vec![0; 7]]
    {
        let mut control = added();
        assert_eq!(control.ingest(VDAGENT_PORT, &bytes), Err(Diagnostic::MalformedControl));
    }
    let mut unready = Control::new();
    assert_eq!(unready.ingest(VDAGENT_PORT, &record(1, 7, 1, VDAGENT_PORT.as_bytes())),
               Err(Diagnostic::MalformedControl));
    for name in [b"".as_slice(), b"bad\0name", b"bad\0\0", b"\xff"]
    {
        let mut control = added();
        assert_eq!(control.ingest(VDAGENT_PORT, &record(1, 7, 1, name)),
                   Err(Diagnostic::MalformedControl));
    }
    let mut control = added();
    control.ingest(VDAGENT_PORT, &record(1, 7, 1, VDAGENT_PORT.as_bytes())).unwrap();
    assert_eq!(control.ingest(VDAGENT_PORT, &record(1, 7, 1, VDAGENT_PORT.as_bytes())),
               Err(Diagnostic::MalformedControl));
    assert!(Diagnostic::MalformedControl.message().contains("out-of-order"));
}
#[test]
fn removal_and_new_control_incarnation_never_reuse_old_connection_state()
{
    let mut control = added();
    assert_eq!(control.ingest(VDAGENT_PORT, &record(1, 2, 1, &[])), Ok(Event::Disconnected));
    let fresh = Control::new();
    assert!(!fresh.connected());
    assert!(!fresh.identified());
    assert!(Diagnostic::Disconnected.message().contains("queued bytes were revoked"));
}
