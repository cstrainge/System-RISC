//! Original-observation-backed handle checks, shared by host and RV64 userspace.
use mac_toolbox::{System,
                  memory::{Handle, LOCKED, PURGEABLE_BIT}};

use super::Failure;
#[derive(Clone, Copy)]
pub struct Observation
{
    pub sequence:    u8,
    pub name:        &'static str,
    pub case:        &'static str,
    pub operation:   &'static str,
    pub error:       i32,
    pub size:        Option<usize>,
    pub size_error:  i32,
    pub state:       Option<u8>,
    pub state_error: i32,
    pub payload:     &'static [u8]
}
#[path = "memory_oracle.rs"]
mod memory_oracle;
pub use memory_oracle::OBSERVATIONS;
fn oracle(case: &str, operation: &str) -> Result<&'static Observation, Failure>
{
    OBSERVATIONS.iter()
                .find(|r| r.case == case && r.operation == operation)
                .ok_or(Failure::Mismatch("missing original Memory observation"))
}
fn require(observation: &Observation, field: &'static str, condition: bool) -> Result<(), Failure>
{
    if condition
    {
        Ok(())
    }
    else
    {
        Err(Failure::Observation { case: observation.name,
                                   field })
    }
}
fn observe(system: &System<'_>, handle: &Handle<'_>, expected: &Observation,
           operation: mac_toolbox::ToolboxResult<()>)
           -> Result<usize, Failure>
{
    require(expected,
            "operation error",
            operation.map_or_else(|e| e.status(), |_| 0) == expected.error)?;
    require(expected, "MemError after operation", system.memory().last_error()? == expected.error)?;
    let length = handle.len();
    require(expected, "GetHandleSize result", match expected.size
    {
        Some(n) => length == Ok(n),
        None => length == Err(mac_toolbox::Error::from_status(expected.size_error))
    })?;
    require(expected,
            "GetHandleSize MemError",
            system.memory().last_error()? == expected.size_error)?;
    let state = handle.state();
    require(expected, "HGetState result", match expected.state
    {
        Some(n) => state == Ok(n),
        None => state == Err(mac_toolbox::Error::from_status(expected.state_error))
    })?;
    require(expected, "HGetState MemError", system.memory().last_error()? == expected.state_error)?;
    if !expected.payload.is_empty()
    {
        let mut bytes = [0xa5; 32];
        let n = handle.read(0, &mut bytes[..expected.payload.len()])?;
        require(expected,
                "known payload",
                n == expected.payload.len() && bytes[..n] == *expected.payload)?;
    }
    Ok(1)
}
fn release(handle: Handle<'_>) -> Result<(), Failure>
{
    handle.set_state(0)?;
    handle.dispose()?;
    Ok(())
}
const SEED: &[u8] = b"0123456789:;<=>?";
const BYTES: &[u8] = &[0xa1, 0xb2, 0xc3, 0xd4];
fn seeded<'a>(system: &System<'a>) -> Result<Handle<'a>, Failure>
{
    let h = system.memory().new_handle_clear(16)?;
    h.write(0, SEED)?;
    Ok(h)
}
pub fn memory_handles(system: &System<'_>) -> Result<usize, Failure>
{
    if OBSERVATIONS.len() != 48
       || OBSERVATIONS.windows(2).any(|pair| pair[0].sequence >= pair[1].sequence)
    {
        return Err(Failure::Mismatch("original Memory trace ordering"));
    }

    let memory = system.memory();
    let mut count = 0;
    for (size, case) in [(0, "new_clear_0"), (1, "new_clear_1"), (16, "new_clear_16")]
    {
        let h = memory.new_handle_clear(size)?;
        count += observe(system, &h, oracle(case, "NewHandleClear")?, Ok(()))?;
        release(h)?;
    }
    let h = memory.new_empty_handle()?;
    count += observe(system, &h, oracle("new_empty", "NewEmptyHandle")?, Ok(()))?;
    let result = h.reallocate(16);
    count += observe(system, &h, oracle("new_empty", "ReallocateHandle16")?, result)?;
    release(h)?;
    let h = seeded(system)?;
    count += observe(system, &h, oracle("state_steps", "initial")?, Ok(()))?;
    let result = h.lock();
    count += observe(system, &h, oracle("state_steps", "HLock")?, result)?;
    let result = h.set_purgeable(true);
    count += observe(system, &h, oracle("state_steps", "HPurge")?, result)?;
    let result = h.unlock();
    count += observe(system, &h, oracle("state_steps", "HUnlock")?, result)?;
    let result = h.set_purgeable(false);
    count += observe(system, &h, oracle("state_steps", "HNoPurge")?, result)?;
    for (state, name) in [(0, "HSetState_00"),
                          (0x20, "HSetState_20"),
                          (0x40, "HSetState_40"),
                          (0x60, "HSetState_60"),
                          (0x80, "HSetState_80"),
                          (0xa0, "HSetState_a0"),
                          (0xc0, "HSetState_c0"),
                          (0xe0, "HSetState_e0"),
                          (0x1f, "HSetState_1f"),
                          (0xff, "HSetState_ff")]
    {
        let result = h.set_state(state);
        count += observe(system, &h, oracle("state_steps", name)?, result)?;
    }
    release(h)?;
    for (locked, targets) in [(false,
                               [(0, "resize_unlocked_0"),
                                (8, "resize_unlocked_8"),
                                (16, "resize_unlocked_16"),
                                (32, "resize_unlocked_32"),
                                (4096, "resize_unlocked_4096")]),
                              (true,
                               [(0, "resize_locked_0"),
                                (8, "resize_locked_8"),
                                (16, "resize_locked_16"),
                                (32, "resize_locked_32"),
                                (4096, "resize_locked_4096")])]
    {
        for (size, case) in targets
        {
            let h = seeded(system)?;
            let guard = memory.new_handle_clear(1024)?;
            guard.lock()?;
            if locked
            {
                h.lock()?;
            }
            let result = h.resize(size);
            count += observe(system, &h, oracle(case, "SetHandleSize")?, result)?;
            release(h)?;
            release(guard)?;
        }
    }
    for (locked, case) in [(false, "empty_unlocked"), (true, "empty_locked")]
    {
        let h = seeded(system)?;
        if locked
        {
            h.lock()?;
        }
        let result = h.empty();
        count += observe(system, &h, oracle(case, "EmptyHandle")?, result)?;
        // Original sequence explicitly unlocks before this reallocation.
        let _ = h.unlock();
        let result = h.reallocate(32);
        count += observe(system, &h, oracle(case, "ReallocateHandle32")?, result)?;
        release(h)?;
    }
    for (locked, case) in [(false, "reallocate_unlocked"), (true, "reallocate_locked")]
    {
        let h = seeded(system)?;
        if locked
        {
            h.lock()?;
        }
        let result = h.reallocate(32);
        count += observe(system, &h, oracle(case, "ReallocateHandle32")?, result)?;
        release(h)?;
    }
    let source = seeded(system)?;
    source.lock()?;
    source.set_purgeable(true)?;
    let copy = source.duplicate()?;
    count += observe(system, &source, oracle("copy", "HandToHand_source")?, Ok(()))?;
    count += observe(system, &copy, oracle("copy", "HandToHand_copy")?, Ok(()))?;
    copy.write(0, &[0x7e])?;
    count += observe(system, &source, oracle("copy", "mutate_copy_source")?, Ok(()))?;
    count += observe(system, &copy, oracle("copy", "mutate_copy_copy")?, Ok(()))?;
    release(copy)?;
    release(source)?;
    let h = memory.handle_from_bytes(BYTES)?;
    count += observe(system, &h, oracle("copy", "PtrToHand4")?, Ok(()))?;
    let result = h.append(BYTES);
    count += observe(system, &h, oracle("copy", "PtrAndHand4")?, result)?;
    release(h)?;
    let source = memory.handle_from_bytes(BYTES)?;
    let destination = memory.handle_from_bytes(BYTES)?;
    let result = destination.append_handle(&source);
    count += observe(system, &source, oracle("copy", "HandAndHand_source")?, result)?;
    count += observe(system, &destination, oracle("copy", "HandAndHand_dest")?, Ok(()))?;
    release(source)?;
    release(destination)?;
    let h = memory.handle_from_bytes(&[])?;
    count += observe(system, &h, oracle("copy", "PtrToHand0")?, Ok(()))?;
    release(h)?;
    for guarded in [false, true]
    {
        let h = seeded(system)?;
        let guard = if guarded
        {
            let guard = memory.new_handle_clear(1024)?;
            guard.lock()?;
            Some(guard)
        }
        else
        {
            None
        };
        h.lock()?;
        let result = h.append(BYTES);
        count += observe(system,
                         &h,
                         oracle("copy",
                                if guarded
                                {
                                    "PtrAndHand_locked_guarded"
                                }
                                else
                                {
                                    "PtrAndHand_locked"
                                })?,
                         result)?;
        release(h)?;
        if let Some(guard) = guard
        {
            release(guard)?;
        }
    }
    let h = seeded(system)?;
    let result = h.replace(BYTES);
    count += observe(system, &h, oracle("copy", "PtrToXHand4")?, result)?;
    release(h)?;
    // These are public constants, not assumptions about private manager layouts.
    if LOCKED != 0x80 || PURGEABLE_BIT != 0x40
    {
        return Err(Failure::Mismatch("public Memory state constants"));
    }
    Ok(count)
}
