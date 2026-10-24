//! Native safety contracts, distinct from original Memory Manager observations.
#![cfg(not(target_arch = "riscv64"))]
mod support;
use mac_abi::memory::*;
use mac_toolbox::Error;
#[test]
fn byte_access_checks_bounds_preserves_buffers_and_keeps_mem_error()
{
    support::with_system(|system| {
        let h = system.memory().handle_from_bytes(b"0123456789abcdef").unwrap();
        assert_eq!(h.write(15, b"xx"), Err(Error::BadArgument));
        let mut outside = [0xa5; 4];
        assert_eq!(h.read(17, &mut outside), Err(Error::BadArgument));
        assert_eq!(outside, [0xa5; 4]);
        assert_eq!(h.read(16, &mut outside), Ok(0));
        assert_eq!(outside, [0xa5; 4]);
        assert!(matches!(system.memory().new_handle_clear(1024 * 1024 + 1),
                         Err(Error::MemoryFull)));
        assert_eq!(system.memory().last_error().unwrap(), Some(Error::MemoryFull));
        let mut bytes = [0; 16];
        assert_eq!(h.read(0, &mut bytes), Ok(16));
        assert_eq!(&bytes, b"0123456789abcdef");
        assert_eq!(system.memory().last_error().unwrap(), Some(Error::MemoryFull));
        assert_eq!(h.len(), Ok(16));
        assert_eq!(system.memory().last_error().unwrap(), None);
    });
}
#[test]
fn disposed_ids_never_alias_reused_slots_and_borrowed_pointers_are_cleared()
{
    support::with_system(|system| {
        let client = system.client();
        let mut call = MemoryCall { operation: FROM_BYTES,
                                    input: usize::MAX as *const u8,
                                    input_length: usize::MAX,
                                    output: usize::MAX as *mut u8,
                                    capacity: usize::MAX,
                                    ..Default::default() };
        client.memory(&mut call, b"ABCD", &mut []).unwrap();
        assert!(call.input.is_null() && call.output.is_null());
        assert_eq!((call.input_length, call.capacity), (0, 0));
        let old = call.handle;
        call.operation = DISPOSE;
        client.memory(&mut call, &[], &mut []).unwrap();
        call.operation = NEW_CLEAR;
        call.size = 4;
        client.memory(&mut call, &[], &mut []).unwrap();
        let current = call.handle;
        assert_ne!(current, old);
        assert_eq!(current & 0xffff, old & 0xffff, "force the same slot");
        // Generations are shared across all domains. Other concurrent sessions
        // may consume intervening generations of this slot; history must advance.
        assert!(current >> 16 > old >> 16);
        call.operation = SIZE;
        call.handle = old;
        assert_eq!(client.memory(&mut call, &[], &mut []), Err(Error::NilHandle));
        assert!(call.input.is_null() && call.output.is_null());
        call.handle = current;
        assert_eq!(client.memory(&mut call, &[], &mut []), Ok(()));
        assert_eq!(call.size, 4);
        call.operation = DISPOSE;
        client.memory(&mut call, &[], &mut []).unwrap();
    });
}
#[test]
fn handles_release_storage_at_scope_exit_and_sessions_are_independent()
{
    for _ in 0..2
    {
        support::with_system(|system| {
            for _ in 0..600
            {
                let h = system.memory().new_handle_clear(8192).unwrap();
                assert_eq!(h.len(), Ok(8192));
            }
            let mut empty = system.memory().new_empty_handle().unwrap();
            assert_eq!(empty.len(), Err(Error::NilHandle));
            empty.dispose().unwrap();
        });
    }
}

#[test]
fn application_cleanup_reclaims_zone_without_reusing_old_handle_generations()
{
    use mac_system::testing::shared::{clear_memory_services, with_memory_services};
    with_memory_services(|services| {
        let system = unsafe { mac_toolbox::System::from_raw(services) }.unwrap();
        let mut old = system.memory().handle_from_bytes(b"old application").unwrap();
        assert_eq!(old.len(), Ok(15));
        clear_memory_services().unwrap();
        assert_eq!(system.memory().last_error().unwrap(), None);
        let mut new = system.memory().handle_from_bytes(b"new application").unwrap();
        let mut output = [0xa5; 15];
        assert_eq!(old.read(0, &mut output), Err(Error::NilHandle));
        assert_eq!(output, [0xa5; 15]);
        assert_eq!(old.write(0, b"bad"), Err(Error::NilHandle));
        assert_eq!(old.try_dispose(), Err(Error::NilHandle));
        assert_eq!(new.read(0, &mut output), Ok(15));
        assert_eq!(&output, b"new application");
        new.try_dispose().unwrap();
        // Each cleanup releases the arena, while metadata prevents stale aliases.
        for _ in 0..600
        {
            let handle = system.memory().new_handle_clear(1024 * 1024).unwrap();
            clear_memory_services().unwrap();
            assert_eq!(handle.len(), Err(Error::NilHandle));
        }
    });
}


#[test]
fn memory_ownership_and_last_error_are_scoped_to_authenticated_processes()
{
    use mac_system::testing::{files::with_owner, inspection as fixture};
    support::with_system(|system| {
        let first = fixture::add("First Memory owner", 0);
        let second = fixture::add("Second Memory owner", 0);
        let mut old = MemoryCall { operation: NEW_CLEAR,
                                   size: 16,
                                   ..Default::default() };
        with_owner(first, || system.client().memory(&mut old, &[], &mut []).unwrap());
        let mut current = MemoryCall { operation: NEW_CLEAR,
                                       size: 16,
                                       ..Default::default() };
        with_owner(second, || {
            system.client().memory(&mut current, &[], &mut []).unwrap();
            assert_ne!(old.handle, current.handle);
            let mut foreign = MemoryCall { operation: SIZE,
                                           handle: old.handle,
                                           ..Default::default() };
            let error = system.client().memory(&mut foreign, &[], &mut []).unwrap_err();
            assert_eq!(error, Error::NilHandle);
            assert!(error.to_string().contains("invalid or empty memory handle"));
            assert_eq!(system.memory().last_error().unwrap(), Some(Error::NilHandle));
        });
        with_owner(first, || {
            assert_eq!(system.memory().last_error().unwrap(), None);
            old.operation = SIZE;
            system.client().memory(&mut old, &[], &mut []).unwrap();
            assert_eq!(old.size, 16);
        });
    });
}

#[test]
fn memory_objects_and_empty_zone_backing_use_the_generic_quarantine_and_retry_walk()
{
    use mac_system::testing::{files::with_owner, inspection as fixture};
    use mac_toolbox::inspection::Scope;
    support::with_system(|system| {
        let owner = fixture::add("Memory cleanup subject", 0);
        with_owner(owner, || {
            let handle = system.memory().handle_from_bytes(b"registered memory").unwrap();
            let membership = system.inspection().snapshot(Scope::Own).unwrap();
            let before = membership.at(0).unwrap();
            assert_eq!((before.live, before.pending), (2, 0));
            let pools = membership.ledger(0).unwrap();
            assert_eq!(pools.len(), 2);
            for i in 0..2
            {
                let pool = pools.at(i).unwrap();
                assert!([*b"MMBL", *b"MMST"].contains(&pool.kind));
                assert_eq!(pool.live, 1);
            }
            let failure = fixture::walk_registered(owner, true, false);
            assert_eq!((failure.report.invalidated, failure.report.quarantined), (2, 2));
            assert!(failure.failures.iter().any(|error| {
                                               error.contains("MMBL")
                                               && error.contains("no registered cleanup provider")
                                           }));
            assert_eq!((membership.at(0).unwrap().live, membership.at(0).unwrap().pending), (0, 2));
            let refused = system.memory().new_empty_handle().err().unwrap();
            assert_eq!(refused, Error::ServiceBusy);
            assert!(refused.to_string().contains("busy"));
            let retry = fixture::walk_registered(owner, false, true);
            assert_eq!((retry.report.released, retry.report.quarantined), (2, 0));
            let done = membership.at(0).unwrap();
            assert_eq!((done.live, done.pending, done.quarantined), (0, 0, 0));
            assert_eq!(done.deaths, before.deaths + 2);
            drop(handle);
        });
    });
}

#[test]
fn first_allocation_refusal_keeps_only_a_registered_error_state_until_the_walk()
{
    use mac_system::testing::{files::with_owner, inspection as fixture};
    use mac_toolbox::inspection::Scope;
    support::with_system(|system| {
        let owner = fixture::add("Empty Memory cleanup subject", 0);
        with_owner(owner, || {
            let error = system.memory().new_handle_clear(1024 * 1024 + 1).err().unwrap();
            assert_eq!(error, Error::MemoryFull);
            assert!(error.to_string().contains("Memory Manager allocation failed"));
            assert_eq!(system.memory().last_error().unwrap(), Some(Error::MemoryFull));
            let snapshot = system.inspection().snapshot(Scope::Own).unwrap();
            assert_eq!((snapshot.at(0).unwrap().live, snapshot.at(0).unwrap().pending), (1, 0));
            let pools = snapshot.ledger(0).unwrap();
            for i in 0..pools.len()
            {
                let pool = pools.at(i).unwrap();
                assert_eq!(pool.live, u64::from(pool.kind == *b"MMST"));
            }
            let result = fixture::walk_registered(owner, false, false);
            assert_eq!((result.report.invalidated,
                        result.report.released,
                        result.report.quarantined),
                       (1, 1, 0));
            assert_eq!((snapshot.at(0).unwrap().live, snapshot.at(0).unwrap().pending), (0, 0));
        });
    });
}
