//! Platform-owned RV64 application scaffold. Application source supplies only a
//! safe run function. Real traps derive allocation authority from the executor;
//! this SDK contains no heap implementation, System forwarding or host fallback.
#[cfg(feature = "application")]
use core::{alloc::{GlobalAlloc, Layout},
           ptr::{self, NonNull}};

#[cfg(feature = "application")]
use crate::{Client, System, SystemServices, ToolboxResult, abi, panic};
mod machine;
pub(crate) use machine::{abort, exit_returned};
/// # Safety
/// The native loader initialized this application image and supplies a readable,
/// aligned service table/callbacks pinned through every application thread and
/// failure path. Entry is called once for this process image.
#[doc(hidden)]
#[cfg(feature = "application")]
pub unsafe fn enter(services: *const SystemServices,
                    run: impl FnOnce(&System<'static>) -> ToolboxResult<()>)
                    -> abi::Status
{
    if let Err(status) = unsafe { panic::install(services) }
    {
        return status;
    }
    let Some(client) = panic::installed_client()
    else
    {
        return abi::BAD_ABI;
    };
    let system = System { client };
    let result = run(&system);
    // Every ordinary no_std application exercises its actual native-thread TLS
    // completion protocol; std CRT replaces this process scaffold separately.
    if let Err(cause) = unsafe { teardown_tls(client) }
    {
        panic::fail(format_args!("Application TLS teardown failed: {cause}"));
    }
    match result
    {
        Ok(()) => abi::OK,
        Err(error) =>
        {
            panic::diagnostic(client,
                              format_args!("APPLICATION_RETURNED_ERROR status={} cause={error}\n",
                                           error.status()));
            error.status()
        }
    }
}

#[cfg(feature = "application")]
fn client() -> Client<'static>
{
    match panic::installed_client()
    {
        Some(client) => client,
        None =>
        {
            machine::abort(format_args!("APPLICATION_RUNTIME_CONTRACT_FAILURE: allocator/failure \
                                         entry before validated services"))
        }
    }
}
#[cfg(feature = "application")]
struct ApplicationAllocator;
#[cfg(feature = "application")]
#[global_allocator]
static ALLOCATOR: ApplicationAllocator = ApplicationAllocator;
#[cfg(feature = "application")]
fn allocation_result(result: mac_traps::application_heap::Result<NonNull<u8>>,
                     operation: &'static str, layout: Layout, new_size: usize)
                     -> *mut u8
{
    match result
    {
        Ok(pointer) => pointer.as_ptr(),
        Err(cause) if cause.is_exhaustion() => ptr::null_mut(),
        Err(cause) =>
        {
            panic::fail(format_args!("Application heap {operation} refused: \
                                      {cause}\nOld/requested layout: {} bytes, alignment {}; new \
                                      size: {new_size}\nTyped status: {}",
                                     layout.size(),
                                     layout.align(),
                                     cause.cause().status()))
        }
    }
}
#[cfg(feature = "application")]
fn validate(layout: Layout, new_size: usize)
{
    if layout.size() == 0
       || new_size == 0
       || Layout::from_size_align(new_size, layout.align()).is_err()
    {
        panic::fail(format_args!("Invalid application allocator caller contract: size={}, \
                                  alignment={}, new_size={new_size}",
                                 layout.size(),
                                 layout.align()));
    }
}
#[cfg(feature = "application")]
unsafe impl GlobalAlloc for ApplicationAllocator
{
    unsafe fn alloc(&self, layout: Layout) -> *mut u8
    {
        validate(layout, layout.size());
        // SAFETY: GlobalAlloc's caller owns the valid nonzero request. The
        // resident still authenticates its real application and validates it.
        allocation_result(unsafe { client().allocate_application(layout, false) },
                          "allocate",
                          layout,
                          layout.size())
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8
    {
        validate(layout, layout.size());
        allocation_result(unsafe { client().allocate_application(layout, true) },
                          "allocate zeroed",
                          layout,
                          layout.size())
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout)
    {
        validate(layout, layout.size());
        let Some(pointer) = NonNull::new(pointer)
        else
        {
            panic::fail(format_args!("Invalid application allocator disposal: null pointer, \
                                      size={}, alignment={}",
                                     layout.size(),
                                     layout.align()));
        };
        // SAFETY: inherited exact live ownership/layout obligation from caller;
        // no observer or allocation fallback is substituted for real disposal.
        if let Err(cause) = unsafe { client().deallocate_application(pointer, layout) }
        {
            panic::fail(format_args!("Application heap disposal refused: \
                                      {cause}\nPointer={pointer:p}, size={}, alignment={}. \
                                      Original resident cleanup remains authoritative.",
                                     layout.size(),
                                     layout.align()));
        }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8
    {
        validate(layout, new_size);
        let Some(pointer) = NonNull::new(pointer)
        else
        {
            panic::fail(format_args!("Invalid application reallocation: null old pointer, \
                                      size={}, alignment={}, new_size={new_size}",
                                     layout.size(),
                                     layout.align()));
        };
        // A returned NO_MEMORY leaves old ownership/layout/data unchanged. Do
        // not deallocate the old block or retry through another allocator.
        allocation_result(unsafe { client().reallocate_application(pointer, layout, new_size) },
                          "reallocate",
                          layout,
                          new_size)
    }
}
#[cfg(feature = "application")]
#[alloc_error_handler]
fn exhausted(layout: Layout) -> !
{
    panic::fail(format_args!("Application heap exhausted (status={}): requested {} bytes, \
                              alignment {}. No fallback allocation was attempted.",
                             abi::NO_MEMORY,
                             layout.size(),
                             layout.align()))
}
#[cfg(feature = "application")]
#[panic_handler]
fn application_panic(info: &core::panic::PanicInfo<'_>) -> !
{
    panic::handle(info)
}

/// # Safety
/// The actual native application thread owns teardown. Resident validates each
/// callback against its pinned Process executable and retains it until ACK.
#[cfg(feature = "application")]
unsafe fn teardown_tls(client: Client<'_>) -> Result<(), mac_traps::native_threads::Failure>
{
    use abi::tls as wire;
    let mut begin = wire::Call { operation: wire::BEGIN,
                                 ..Default::default() };
    unsafe {
        client.tls(&mut begin)?;
    }
    loop
    {
        let mut next = wire::Call { operation: wire::NEXT,
                                    ..Default::default() };
        unsafe {
            client.tls(&mut next)?;
        }
        if next.present == 0
        {
            break;
        }
        if next.destructor == 0 || next.ticket == 0
        {
            return Err(mac_traps::native_threads::Failure::Protocol("NEXT omitted callback \
                                                                     receipt"));
        }
        let callback: unsafe extern "C" fn(*mut u8) =
            unsafe { core::mem::transmute(next.destructor) };
        unsafe {
            callback(next.value as *mut u8);
        }
        let mut ack = wire::Call { operation: wire::ACK,
                                   ticket: next.ticket,
                                   ..Default::default() };
        unsafe {
            client.tls(&mut ack)?;
        }
    }
    let mut finish = wire::Call { operation: wire::FINISH,
                                  ..Default::default() };
    unsafe { client.tls(&mut finish) }
}
