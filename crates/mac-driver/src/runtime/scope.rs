//! One audited synchronous C thunk for native module allocation/exclusion scopes.
use core::{ffi::c_void, ptr};

use mac_abi::{BAD_ABI, OK};

use crate::{Error, Result, abi};

pub(super) fn invoke<R, F: FnOnce() -> R>(context: u64, callback: abi::WithModuleAllocation,
                                          operation: F)
                                          -> Result<R>
{
    struct State<F, R>
    {
        operation: Option<F>,
        result:    Option<R>
    }
    unsafe extern "C" fn invoke<F: FnOnce() -> R, R>(pointer: *mut c_void) -> i32
    {
        // SAFETY: the synchronous resident contract borrows this exact stack
        // record exclusively; no Rust trait object or result crosses the ABI.
        let state = unsafe { &mut *pointer.cast::<State<F, R>>() };
        let Some(operation) = state.operation.take()
        else
        {
            return BAD_ABI;
        };
        state.result = Some(operation());
        OK
    }
    let mut state = State { operation: Some(operation),
                            result:    None };
    // SAFETY: entry checked this immutable resident callback. The stack state
    // and generic thunk remain live until its synchronous return.
    let status = unsafe { callback(context, invoke::<F, R>, ptr::from_mut(&mut state).cast()) };
    if status != OK
    {
        return Err(Error(if status < 0 { status } else { BAD_ABI }));
    }
    if state.operation.is_some()
    {
        return Err(Error(BAD_ABI));
    }
    state.result.take().ok_or(Error(BAD_ABI))
}
