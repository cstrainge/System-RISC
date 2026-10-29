//! Native resource FFI adapter. Each loaded definition owns its implementation.
use mac_abi::{DefinitionCall, DefinitionEntry};

pub(crate) fn invoke(entry: DefinitionEntry, call: &mut DefinitionCall) -> i64
{
    // Loader-validated entry; the manager supplies this family's live borrowed state.
    #[cfg(target_arch = "riscv64")]
    {
        crate::context::current_thread_context().definitions
                                                .with(call.kind, call.id, call.surface, || unsafe {
                                                    entry(call)
                                                })
    }
    #[cfg(not(target_arch = "riscv64"))]
    unsafe {
        entry(call)
    }
}
