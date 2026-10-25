use mac_object_table::{Kind, Table};
#[test]
fn memory_wire_format_and_history_survive_remove_and_clear()
{
    let mut table = Table::new(Kind::Memory);
    let first = table.insert(17).unwrap();
    assert!((1..=512).contains(&(first & 0xffff)));
    assert_ne!(first >> 16, 0);
    assert_eq!(table.remove(first), Some(17));
    let second = table.insert(18).unwrap();
    assert_ne!(second, first);
    assert!((1..=512).contains(&(second & 0xffff)));
    assert!(table.get(first).is_none());
    table.clear();
    let third = table.insert(19).unwrap();
    assert_ne!(third, second);
    assert!((1..=512).contains(&(third & 0xffff)));
    assert!(table.get(first).is_none());
    assert!(table.get(second).is_none());
}
#[test]
fn service_namespaces_are_disjoint_including_host_wire_leases()
{
    let kinds = [Kind::Memory,
                 Kind::Files,
                 Kind::FileWrites,
                 Kind::Volumes,
                 Kind::Modules,
                 Kind::DriverInterfaces,
                 Kind::DriverRequests,
                 Kind::HostFiles,
                 Kind::HostDirectories,
                 Kind::HostTransactions,
                 Kind::HostSessions,
                 Kind::DriverDevices,
                 Kind::Threads,
                 Kind::DriverJobs,
                 Kind::DriverDma,
                 Kind::FileSources,
                 Kind::HostNodes,
                 Kind::FileWatches,
                 Kind::Drags,
                 Kind::DragReceivers,
                 Kind::FileStreams,
                 Kind::FileGroups,
                 Kind::FileTransfers];
    let mut tables: Vec<_> = kinds.into_iter().map(Table::new).collect();
    let ids: Vec<_> = tables.iter_mut().map(|t| t.insert(1).unwrap()).collect();
    for (row, table) in tables.iter().enumerate()
    {
        for (col, id) in ids.iter().enumerate()
        {
            assert_eq!(table.get(*id).is_some(), row == col);
        }
    }
}
#[test]
fn exhaustion_retires_slots_instead_of_wrapping_to_a_stale_id()
{
    let mut table = Table::new(Kind::Memory);
    let first = table.insert(1).unwrap();
    table.remove(first);
    for _ in 1..u16::MAX
    {
        let id = table.insert(1).unwrap();
        assert_ne!(id, first);
        table.remove(id);
    }
    let next = table.insert(2).unwrap();
    assert_ne!(first & 0xffff, next & 0xffff);
    assert!(table.get(first).is_none());
    table.clear();
    let next_generation = table.insert(3).unwrap();
    assert_ne!(next, next_generation);
    assert!(table.get(next).is_none());
}
#[test]
fn distinct_host_sessions_cannot_accept_each_others_file_ids()
{
    let mut first = Table::new(Kind::HostFiles);
    let mut second = Table::new(Kind::HostFiles);
    let a = first.insert(1).unwrap();
    let b = second.insert(2).unwrap();
    assert_ne!(a, b);
    assert!(first.get(b).is_none());
    assert!(second.get(a).is_none());
    first.remove(a);
    let c = first.insert(3).unwrap();
    assert_ne!(a, c);
    assert_ne!(b, c);
    assert!(first.get(a).is_none());
}

#[test]
fn retired_thread_slots_cannot_alias_recreated_tables()
{
    for kind in [Kind::Threads]
    {
        let mut threads = Table::new(kind);
        let retired = threads.insert("retired stack").unwrap();
        assert_eq!(threads.remove(retired), Some("retired stack"));
        let replacement = threads.insert("new stack").unwrap();
        assert_eq!(retired & 0xffff, replacement & 0xffff);
        assert_ne!(retired, replacement);
        assert!(threads.get(retired).is_none());
        let mut other = Table::new(kind);
        let foreign = other.insert("another scheduler").unwrap();
        assert_ne!(foreign, replacement);
        assert!(threads.get(foreign).is_none());
        threads.clear();
        let fresh = threads.insert("fresh stack").unwrap();
        assert_ne!(fresh, replacement);
        assert!(threads.get(replacement).is_none());
    }
}

#[test]
fn memory_projections_reject_foreign_owners_and_recreated_tables()
{
    let mut first = Table::new(Kind::Memory);
    let mut second = Table::new(Kind::Memory);
    let old = first.insert("first owner").unwrap();
    let foreign = second.insert("second owner").unwrap();
    assert_ne!(old, foreign);
    assert!(first.get(foreign).is_none());
    assert!(second.get(old).is_none());
    drop(first);
    let mut replacement = Table::new(Kind::Memory);
    let current = replacement.insert("replacement owner").unwrap();
    assert_ne!(old, current);
    assert_ne!(foreign, current);
    assert!(replacement.get(old).is_none());
    assert!(second.get(current).is_none());
}

#[test]
fn file_manager_ids_reject_foreign_instances_and_reconstructed_storage()
{
    for kind in [Kind::Files, Kind::FileWrites, Kind::Volumes]
    {
        let mut first = Table::new(kind);
        let mut foreign = Table::new(kind);
        let stale = first.insert("old").unwrap();
        let other = foreign.insert("foreign").unwrap();
        assert_ne!(stale, other);
        assert!(first.get(other).is_none());
        assert!(foreign.get(stale).is_none());
        assert!(first.remove(other).is_none());
        assert_eq!(first.get(stale), Some(&"old"));
        drop(first);
        let mut recreated = Table::new(kind);
        let fresh = recreated.insert("fresh").unwrap();
        assert_eq!(stale & 0xffff, fresh & 0xffff);
        assert_ne!(stale, fresh);
        assert_ne!(other, fresh);
        assert!(recreated.get(stale).is_none());
        assert!(recreated.remove(stale).is_none());
        assert!(recreated.get(other).is_none());
        assert_eq!(foreign.get(other), Some(&"foreign"));
        recreated.clear();
        let next = recreated.insert("after reset").unwrap();
        assert_ne!(fresh, next);
        assert!(recreated.get(fresh).is_none());
        assert_eq!(recreated.get(next), Some(&"after reset"));
    }
}
