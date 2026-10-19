//! Native safety contracts, distinct from original Memory Manager observations.
#![cfg(not(target_arch = "riscv64"))]
mod support;
use mac_abi::{BAD_ARGUMENT, memory::*};
use mac_toolbox::Error;
#[test]
fn byte_access_checks_bounds_preserves_buffers_and_keeps_mem_error()
{
    support::with_system(|system| {
        let h = system.memory().handle_from_bytes(b"0123456789abcdef").unwrap();
        assert_eq!(h.write(15, b"xx"), Err(Error(BAD_ARGUMENT)));
        let mut outside = [0xa5; 4];
        assert_eq!(h.read(17, &mut outside), Err(Error(BAD_ARGUMENT)));
        assert_eq!(outside, [0xa5; 4]);
        assert_eq!(h.read(16, &mut outside), Ok(0));
        assert_eq!(outside, [0xa5; 4]);
        assert!(matches!(system.memory().new_handle_clear(1024 * 1024 + 1),
                         Err(Error(MEM_FULL_ERR))));
        assert_eq!(system.memory().last_error().unwrap(), MEM_FULL_ERR);
        let mut bytes = [0; 16];
        assert_eq!(h.read(0, &mut bytes), Ok(16));
        assert_eq!(&bytes, b"0123456789abcdef");
        assert_eq!(system.memory().last_error().unwrap(), MEM_FULL_ERR);
        assert_eq!(h.len(), Ok(16));
        assert_eq!(system.memory().last_error().unwrap(), 0);
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
        call.operation = SIZE;
        call.handle = old;
        assert_eq!(client.memory(&mut call, &[], &mut []), Err(Error(NIL_HANDLE_ERR)));
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
            let empty = system.memory().new_empty_handle().unwrap();
            assert_eq!(empty.len(), Err(Error(NIL_HANDLE_ERR)));
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
        assert_eq!(system.memory().last_error().unwrap(), 0);
        let mut new = system.memory().handle_from_bytes(b"new application").unwrap();
        let mut output = [0xa5; 15];
        assert_eq!(old.read(0, &mut output), Err(Error(NIL_HANDLE_ERR)));
        assert_eq!(output, [0xa5; 15]);
        assert_eq!(old.write(0, b"bad"), Err(Error(NIL_HANDLE_ERR)));
        assert_eq!(old.try_dispose(), Err(Error(NIL_HANDLE_ERR)));
        assert_eq!(new.read(0, &mut output), Ok(15));
        assert_eq!(&output, b"new application");
        new.try_dispose().unwrap();
        // Each cleanup releases the arena, while metadata prevents stale aliases.
        for _ in 0..600
        {
            let handle = system.memory().new_handle_clear(1024 * 1024).unwrap();
            clear_memory_services().unwrap();
            assert_eq!(handle.len(), Err(Error(NIL_HANDLE_ERR)));
        }
    });
}
