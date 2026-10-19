//! Native resource FFI adapter. Each loaded definition owns its implementation.
use mac_abi::{DefinitionCall, DefinitionEntry};

pub(crate) fn invoke(entry: DefinitionEntry, call: &mut DefinitionCall) -> i64
{
    // Loader-validated entry; the manager supplies this family's live borrowed state.
    unsafe { entry(call) }
}
