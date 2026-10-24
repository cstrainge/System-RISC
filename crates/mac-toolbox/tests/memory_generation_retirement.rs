//! Run exhaustion in its own test process: the production projection issuer
//! deliberately retains retired generations for the lifetime of this process.
#![cfg(not(target_arch = "riscv64"))]
mod support;
use mac_abi::memory::*;
use mac_toolbox::Error;

#[test]
fn full_u16_generation_range_retires_the_slot_without_reissuing_an_old_id()
{
    support::with_system(|system| {
        let client = system.client();
        let mut call = MemoryCall { operation: NEW_EMPTY,
                                    ..Default::default() };
        client.memory(&mut call, &[], &mut []).unwrap();
        let first = call.handle;
        let slot = first & 0xffff;
        loop
        {
            call.operation = DISPOSE;
            client.memory(&mut call, &[], &mut []).unwrap();
            call.operation = NEW_EMPTY;
            client.memory(&mut call, &[], &mut []).unwrap();
            if call.handle & 0xffff != slot
            {
                break;
            }
            assert_ne!(call.handle, first);
        }
        let current = call.handle;
        assert_eq!(current & 0xffff, slot + 1, "retire rather than wrap generation");
        call.operation = REALLOCATE;
        call.size = 4;
        client.memory(&mut call, &[], &mut []).unwrap();
        for stale in [first, (0xffff << 16) | slot, slot, 0, u32::MAX]
        {
            call.operation = DISPOSE;
            call.handle = stale;
            assert_eq!(client.memory(&mut call, &[], &mut []), Err(Error::NilHandle));
        }
        call.operation = SIZE;
        call.handle = current;
        client.memory(&mut call, &[], &mut []).unwrap();
        assert_eq!(call.size, 4);
        call.operation = DISPOSE;
        client.memory(&mut call, &[], &mut []).unwrap();
    });
}
