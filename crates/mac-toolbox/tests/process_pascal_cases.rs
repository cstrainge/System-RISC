//! Paired native/Pascal Process Manager trap acceptance. Raw conversion traps
//! remain absent from the new Rust SDK, so this boundary test uses the raw gate.
#![cfg(target_arch = "riscv64")]
use mac_abi::{BAD_ARGUMENT, TrapGate,
              processes::{CURRENT, INDEX, ProcessCall},
              traps::PSTRING_PROCESS};
use mac_toolbox::System;

use crate::process_cases::Failure;
fn require(value: bool, message: &'static str) -> Result<(), Failure>
{
    if value { Ok(()) } else { Err(Failure::Contract(message)) }
}
fn invoke(gate: TrapGate, call: &mut ProcessCall, size: usize) -> i64
{
    // SAFETY: this live, exclusively borrowed typed request has no nested pointers.
    // A deliberately short advertised size tests rejection without invalid memory.
    unsafe { gate(PSTRING_PROCESS, core::ptr::from_mut(call).cast(), size) }
}
pub fn current_name(system: &System<'_>, gate: TrapGate) -> Result<usize, Failure>
{
    let native = system.processes().current()?;
    require(native.name().is_ascii(), "Pascal process fixture uses ASCII application name")?;
    let mut call = ProcessCall { operation: CURRENT,
                                 ..Default::default() };
    let size = core::mem::size_of::<ProcessCall>();
    require(invoke(gate, &mut call, size) == 0, "Pascal current process trap")?;
    let n = native.name().len();
    require(call.info.id == native.id
            && call.info.thread == native.thread
            && call.info.name_length as usize == n + 1
            && call.info.name[0] as usize == n
            && &call.info.name[1..n + 1] == native.name().as_bytes(),
            "Pascal current process name conversion")?;
    require(system.processes().current()?.name() == native.name(),
            "Pascal conversion mutated native process name")?;
    call = ProcessCall { operation: INDEX,
                         index: 0,
                         ..Default::default() };
    require(invoke(gate, &mut call, size) == 0
            && call.info.id == native.id
            && call.info.name[0] as usize == n,
            "Pascal indexed process name conversion")?;
    call.index = u32::MAX;
    call.info.name.fill(0xa5);
    require(invoke(gate, &mut call, size) < 0 && call.info.name.iter().all(|&b| b == 0xa5),
            "invalid Pascal process index preserves output")?;
    require(invoke(gate, &mut call, size - 1) == BAD_ARGUMENT as i64
            && call.info.name.iter().all(|&b| b == 0xa5),
            "short Pascal process request rejected atomically")?;
    system.log("PROCESS_PASCAL_PASS cases=5\n")?;
    Ok(5)
}
