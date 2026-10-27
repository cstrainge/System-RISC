//! Real lifetime ledgers select presentation membership, never mutable metadata IDs.
use alloc::string::ToString;

use mac_object_table::domain::{Directory, DomainKind, OwnerIndex};

use super::*;
use crate::{testing::allocations::Observe, toolbox::handles::InvocationGate};

#[test]
fn hide_others_preserves_stopped_and_provisional_metadata_and_uses_ledger_identity()
{
    let mut directory = Directory::<ProcessInfo, InvocationGate>::new();
    let mut add = |thread| {
        directory.insert(OwnerIndex::issue(DomainKind::Process).unwrap(),
                         ProcessInfo { thread,
                                       ..Default::default() })
                 .unwrap()
                 .incarnation()
    };
    let foreground = add(11);
    let other = add(12);
    let stopping = add(13);
    let provisional = add(0);
    directory.stop_admission(stopping).unwrap();
    // A payload ID can neither impersonate the foreground ledger nor substitute
    // another ledger for the actual foreground process.
    directory.get_mut(foreground).unwrap().id = other;
    directory.get_mut(other).unwrap().id = foreground;
    let stopped_before = *directory.get(stopping).unwrap().value();
    let provisional_before = *directory.get(provisional).unwrap().value();
    let current = directory.get(foreground).unwrap();
    assert_eq!(require_foreground(current.ledger(), current.value()), Ok(()));
    let observe = Observe::begin(true, None);
    let mut changed = 0;
    for (ledger, process) in directory.entries_mut()
    {
        changed += usize::from(hide_other(ledger, process, foreground));
    }
    let allocations = observe.finish();
    assert_eq!(allocations, 0);
    assert_eq!(changed, 1);
    assert_eq!(directory.get(foreground).unwrap().value().hidden, 0);
    assert_eq!(directory.get(other).unwrap().value().hidden, 1);
    let metadata = |i: &ProcessInfo| {
        (i.id,
         i.thread,
         i.image,
         i.volume,
         i.cnid,
         i.foreground,
         i.hidden,
         i.name,
         i.name_length,
         i.switches)
    };
    assert_eq!(metadata(directory.get(stopping).unwrap().value()), metadata(&stopped_before));
    assert_eq!(metadata(directory.get(provisional).unwrap().value()),
               metadata(&provisional_before));
    assert!(!running(directory.get(stopping).unwrap().ledger(), &stopped_before));
    assert!(!running(directory.get(provisional).unwrap().ledger(), &provisional_before));
    let observe = Observe::begin(true, None);
    let changed =
        directory.entries_mut().any(|(ledger, process)| hide_other(ledger, process, foreground));
    let allocations = observe.finish();
    assert!(!changed);
    assert_eq!(allocations, 0);
}

#[test]
fn hidden_stopped_and_provisional_foregrounds_refuse_before_hiding_anything()
{
    let mut directory = Directory::<ProcessInfo, InvocationGate>::new();
    let foreground = directory.insert(OwnerIndex::issue(DomainKind::Process).unwrap(),
                                      ProcessInfo { thread: 11,
                                                    ..Default::default() })
                              .unwrap()
                              .incarnation();
    let provisional = directory.insert(OwnerIndex::issue(DomainKind::Process).unwrap(),
                                       ProcessInfo::default())
                               .unwrap()
                               .incarnation();
    let entry = directory.get(provisional).unwrap();
    assert_eq!(require_foreground(entry.ledger(), entry.value()), Err(SERVICE_BUSY));
    directory.get_mut(foreground).unwrap().hidden = 1;
    let entry = directory.get(foreground).unwrap();
    assert_eq!(require_foreground(entry.ledger(), entry.value()), Err(ACCESS_DENIED));
    directory.get_mut(foreground).unwrap().hidden = 0;
    directory.stop_admission(foreground).unwrap();
    let entry = directory.get(foreground).unwrap();
    assert_eq!(require_foreground(entry.ledger(), entry.value()), Err(SERVICE_BUSY));
    assert_eq!(directory.get(provisional).unwrap().value().hidden, 0);
    assert!(mac_traps::Error::from_status(SERVICE_BUSY).to_string().contains("busy"));
    assert!(mac_traps::Error::from_status(ACCESS_DENIED).to_string().contains("protected"));
}
