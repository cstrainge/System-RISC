//! DITL UTF-8 substitution admission through the public native Dialog Manager.
#![cfg(not(target_arch = "riscv64"))]
#[path = "support/allocations.rs"]
mod allocations;
use allocations::Observe;
use mac_formats::resource::{Resource, encode};
use mac_toolbox::{Error, ResourceScope, System};

fn alert(payload: &[u8]) -> Vec<u8>
{
    let mut alrt = Vec::new();
    for value in [40i16, 20, 140, 340, 7099, 0x4444]
    {
        alrt.extend_from_slice(&value.to_be_bytes());
    }
    let mut items = vec![0, 0];
    items.extend_from_slice(&[0; 4]);
    for value in [10i16, 10, 70, 300]
    {
        items.extend_from_slice(&value.to_be_bytes());
    }
    items.extend_from_slice(&[8, payload.len().try_into().unwrap()]);
    items.extend_from_slice(payload);
    if !items.len().is_multiple_of(2)
    {
        items.push(0);
    }
    let row = |kind, data| {
        Resource { kind,
                   id: 7099,
                   name: None,
                   attributes: 0,
                   data }
    };
    encode(&[row(*b"ALRT", alrt), row(*b"DITL", items)]).unwrap()
}
#[test]
fn alert_substitution_exhaustion_is_typed_and_retry_uses_the_same_resources()
{
    let system = encode(&[]).unwrap();
    let app = alert(b"Caf\x8e ^0 and ^0");
    mac_system::testing::with_resource_forks(&system, &app, |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let dialogs = system.dialogs();
        let observe = Observe::begin(false, None);
        let validated = dialogs.validate_alert(ResourceScope::Application, 7099, "🦀 ^0");
        let count = observe.finish();
        assert_eq!(validated, Ok(1));
        assert!(count >= 2, "exercise item table and final substituted text admission");
        for ordinal in 1..=count
        {
            let observe = Observe::fail_at(ordinal);
            let refused = dialogs.validate_alert(ResourceScope::Application, 7099, "🦀 ^0");
            let attempts = observe.finish();
            assert_eq!(refused, Err(Error::NoMemory), "refused allocation {ordinal}/{count}");
            assert!(attempts >= ordinal);
            assert_eq!(dialogs.validate_alert(ResourceScope::Application, 7099, "🦀 ^0"), Ok(1));
        }
        let observe = Observe::begin(true, None);
        let missing = dialogs.validate_alert(ResourceScope::Application, 7100, "");
        let count = observe.finish();
        assert_eq!(missing, Err(Error::NotFound));
        assert_eq!(count, 0);
        assert!(Error::NoMemory.to_string().contains("memory"));
        assert!(Error::NotFound.to_string().contains("found"));
    }).unwrap();
}
#[test]
fn overlength_substitution_fails_before_text_backing_then_validates_again()
{
    let system = encode(&[]).unwrap();
    let app = alert(b"^0^0");
    let text = "é".repeat(mac_abi::INLINE_STRING_BYTES / 2);
    mac_system::testing::with_resource_forks(&system, &app, |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let dialogs = system.dialogs();
        let observe = Observe::fail_at(2);
        let invalid = dialogs.validate_alert(ResourceScope::Application, 7099, &text);
        let count = observe.finish();
        assert_eq!(invalid, Err(Error::BadArgument));
        assert_eq!(count, 1, "item table may be admitted; oversized text must not be allocated");
        assert_eq!(dialogs.validate_alert(ResourceScope::Application, 7099, "é"), Ok(1));
        assert!(Error::BadArgument.to_string().contains("argument"));
    }).unwrap();
}
