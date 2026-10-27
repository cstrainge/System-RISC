//! Native in-kernel module SDK. Ordinary driver logic uses borrowed Rust slices;
//! this module is the narrow, audited C/pointer boundary. This is not an app SDK.
#![no_std]
pub mod devices;
#[doc(hidden)]
pub mod runtime;
pub mod file_sources;
/// Platform panic scaffold: diagnostics followed by System's loud privileged
/// fault shutdown. This does not claim an unproven module unwind contract.
#[macro_export]
macro_rules! driver_panic_handler {
    () => {
        #[panic_handler]
        fn __driver_panic(info: &core::panic::PanicInfo<'_>) -> !
        {
            $crate::runtime::panic(info)
        }
    };
}
use mac_abi::{BAD_ABI, BAD_ARGUMENT, NOT_SUPPORTED, OK, Status};
pub use mac_module::{ClassId, Offer, abi};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error(pub Status);
pub type Result<T> = core::result::Result<T, Error>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase
{
    Start,
    Poll,
    Cancel
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Completion
{
    Complete(usize),
    Pending
}
#[derive(Clone, Copy, Debug)]
pub struct Request<'a>
{
    pub ticket:    u64,
    pub phase:     Phase,
    pub class:     ClassId,
    pub operation: u32,
    pub input:     &'a [u8]
}
/// Callbacks must make bounded progress, retain no borrowed buffers and return
/// Pending instead of waiting for hardware. No callback runs under registry
/// locks. Drivers synchronize their own state; pinning code does not serialize it.
pub trait Driver: Sync
{
    fn initialize(&self, host: Client) -> Result<()>;
    fn request(&self, request: Request<'_>, output: &mut [u8]) -> Result<Completion>;
    fn quiesce(&self) -> Result<()>
    {
        Ok(())
    }
}
#[derive(Clone, Copy)]
pub struct Client
{
    host: abi::Host
}
impl Client
{
    pub fn module(self) -> u64
    {
        self.host.module
    }

    pub fn resolve(self, class: ClassId, version: u32) -> Result<Provider>
    {
        let resolve = self.host.resolve.ok_or(Error(NOT_SUPPORTED))?;
        let mut provider = 0;
        // The resident callback validates this module's declared dependencies.
        let status = unsafe {
            resolve(self.host.context, u32::from_le_bytes(class.0), version, &mut provider)
        };
        if status == OK && provider != 0
        {
            Ok(Provider { client: self,
                          handle: provider,
                          class })
        }
        else
        {
            Err(Error(if status == OK { BAD_ABI } else { status }))
        }
    }
}
/// A checked reference to one declared dependency. The resident validates its
/// generation and the originating module's authority on every invocation.
#[derive(Clone, Copy)]
pub struct Provider
{
    client: Client,
    handle: u64,
    class:  ClassId
}
impl Provider
{
    /// Invoke an admitted resident operation that cannot defer. System creates
    /// no broker ticket on this path; a busy result is safe to retry unchanged.
    /// Modules and operations without this explicit contract are refused before
    /// dispatch. Device reset must use the ticket-owning asynchronous method.
    pub(crate) fn synchronous(self, operation: u32, input: &[u8], output: &mut [u8])
                              -> Result<usize>
    {
        let mut call =
            abi::Call { header: abi::Header::of::<abi::Call>(),
                        ticket: 0,
                        phase: abi::SYNCHRONOUS,
                        class: self.class.0,
                        operation,
                        input: input.as_ptr(),
                        input_len: input.len().try_into().map_err(|_| Error(BAD_ARGUMENT))?,
                        output_capacity: output.len()
                                               .try_into()
                                               .map_err(|_| Error(BAD_ARGUMENT))?,
                        output: output.as_mut_ptr(),
                        output_len: 0,
                        reserved: 0 };
        let broker = self.client.host.call.ok_or(Error(NOT_SUPPORTED))?;
        // Same admitted host and exclusive borrowed buffers as invoke(). The
        // synchronous resident contract cannot publish a deferred operation.
        let status = unsafe { broker(self.client.host.context, self.handle, &mut call) };
        if call.ticket != 0 || call.output_len as usize > output.len() || status > OK
        {
            return Err(Error(BAD_ABI));
        }
        if status == OK { Ok(call.output_len as usize) } else { Err(Error(status)) }
    }

    /// A provider handle is issued by the resident registry, never an address.
    /// The resident validates the declaring module's dependency/service on every
    /// call. Pending and pre-dispatch contention retain a live ticket; terminal
    /// completion clears it. Never discard a pending ticket before cancellation
    /// is acknowledged.
    pub fn invoke(self, ticket: &mut u64, phase: Phase, operation: u32, input: &[u8],
                  output: &mut [u8])
                  -> Result<Completion>
    {
        if input.len() > 1024 * 1024 || output.len() > 1024 * 1024
        {
            return Err(Error(BAD_ARGUMENT));
        }
        let mut call = abi::Call { header: abi::Header::of::<abi::Call>(),
                                   ticket: *ticket,
                                   phase: match phase
                                   {
                                       Phase::Start => abi::START,
                                       Phase::Poll => abi::POLL,
                                       Phase::Cancel => abi::CANCEL
                                   },
                                   class: self.class.0,
                                   operation,
                                   input: input.as_ptr(),
                                   input_len: input.len() as u32,
                                   output_capacity: output.len() as u32,
                                   output: output.as_mut_ptr(),
                                   output_len: 0,
                                   reserved: 0 };
        let broker = self.client.host.call.ok_or(Error(NOT_SUPPORTED))?;
        // Client can only be created from a host descriptor admitted by entry().
        // The slices are exclusively borrowed for this synchronous callback.
        let status = unsafe { broker(self.client.host.context, self.handle, &mut call) };
        *ticket = call.ticket;
        match status
        {
            OK if call.output_len as usize <= output.len() =>
            {
                Ok(Completion::Complete(call.output_len as usize))
            }
            abi::PENDING if call.output_len == 0 => Ok(Completion::Pending),
            OK | abi::PENDING => Err(Error(BAD_ABI)),
            value => Err(Error(value))
        }
    }
}
fn aligned<T>(pointer: *const T) -> bool
{
    !pointer.is_null() && pointer.addr().is_multiple_of(core::mem::align_of::<T>())
}
/// # Safety
/// Host and output point to live, aligned resident buffers for these exact
/// types. Host callback/context remain valid until reboot. Dispatch belongs to
/// this pinned module. The resident must serialize initialization.
#[doc(hidden)]
pub unsafe fn entry(driver: &impl Driver, host: *const abi::Host, output: *mut abi::Instance,
                    dispatch: abi::Dispatch)
                    -> Status
{
    if !aligned(host) || !aligned(output)
    {
        return BAD_ARGUMENT;
    }
    let host = unsafe { *host };
    if !host.header.valid::<abi::Host>() || host.reserved != 0 || host.module == 0
    {
        return BAD_ABI;
    }
    match driver.initialize(Client { host })
    {
        Ok(()) =>
        {
            unsafe {
                output.write(abi::Instance { header:   abi::Header::of::<abi::Instance>(),
                                             state:    0,
                                             dispatch: Some(dispatch) })
            };
            OK
        }
        Err(Error(status)) =>
        {
            if status < 0
            {
                status
            }
            else
            {
                BAD_ABI
            }
        }
    }
}
/// # Safety
/// Call and its declared input/output buffers are live for this invocation.
/// Output is exclusive and does not alias Call or input. No borrowed buffer is
/// retained after return, including on Pending. The module remains pinned.
#[doc(hidden)]
pub unsafe fn dispatch(driver: &impl Driver, state: u64, pointer: *mut abi::Call) -> Status
{
    if state != 0 || !aligned(pointer)
    {
        return BAD_ARGUMENT;
    }
    let call = unsafe { &mut *pointer };
    if !call.header.valid::<abi::Call>() || call.reserved != 0
    {
        return BAD_ABI;
    }
    call.output_len = 0;
    if call.input_len > 1024 * 1024 || call.output_capacity > 1024 * 1024
    {
        return BAD_ARGUMENT;
    }
    if call.phase == abi::QUIESCE
    {
        if call.class != [0; 4]
           || call.operation != 0
           || call.ticket != 0
           || call.input_len != 0
           || call.output_capacity != 0
        {
            return BAD_ARGUMENT;
        }
        return match driver.quiesce()
        {
            Ok(()) => OK,
            Err(Error(status)) =>
            {
                if status < 0
                {
                    status
                }
                else
                {
                    BAD_ABI
                }
            }
        };
    }
    let phase = match call.phase
    {
        abi::START => Phase::Start,
        abi::POLL => Phase::Poll,
        abi::CANCEL => Phase::Cancel,
        _ => return BAD_ARGUMENT
    };
    if call.ticket == 0 || call.class == [0; 4]
    {
        return BAD_ARGUMENT;
    }
    if call.input_len != 0 && call.input.is_null()
       || call.output_capacity != 0 && call.output.is_null()
    {
        return BAD_ARGUMENT;
    }
    let Some(input_end) = call.input.addr().checked_add(call.input_len as usize)
    else
    {
        return BAD_ARGUMENT;
    };
    let Some(output_end) = call.output.addr().checked_add(call.output_capacity as usize)
    else
    {
        return BAD_ARGUMENT;
    };
    if call.input_len != 0
       && call.output_capacity != 0
       && call.input.addr() < output_end
       && call.output.addr() < input_end
    {
        return BAD_ARGUMENT;
    }
    let input = if call.input_len == 0
    {
        &[]
    }
    else
    {
        unsafe { core::slice::from_raw_parts(call.input, call.input_len as usize) }
    };
    let output = if call.output_capacity == 0
    {
        &mut []
    }
    else
    {
        unsafe { core::slice::from_raw_parts_mut(call.output, call.output_capacity as usize) }
    };
    match driver.request(Request { ticket: call.ticket,
                                   phase,
                                   class: ClassId(call.class),
                                   operation: call.operation,
                                   input },
                         output)
    {
        Ok(Completion::Complete(length)) if length <= output.len() =>
        {
            call.output_len = length as u32;
            OK
        }
        Ok(Completion::Complete(_)) => BAD_ABI,
        Ok(Completion::Pending) => abi::PENDING,
        Err(Error(status)) =>
        {
            if status < 0
            {
                status
            }
            else
            {
                BAD_ABI
            }
        }
    }
}
/// Emit the relocatable entry and linked build contract. The driver body stays
/// safe Rust; package generation verifies this contract against the linked SDK.
#[macro_export]
macro_rules! export_driver {
    ($driver:expr) => {
        #[used]
        #[unsafe(link_section = ".system_risc_module")]
        static MODULE_CONTRACT: [u8; include_bytes!(concat!(env!("OUT_DIR"),
                                                              "/module_contract.bin")).len()] =
            *include_bytes!(concat!(env!("OUT_DIR"), "/module_contract.bin"));
        #[unsafe(no_mangle)]
        #[unsafe(link_section = ".text.entry")]
        unsafe extern "C" fn _start(host: *const $crate::abi::Host,
                                    output: *mut $crate::abi::Instance)
                                    -> i32
        {
            // The resident package loader validates and pins this code before entry.
            unsafe { $crate::entry(&$driver, host, output, module_dispatch) }
        }
        unsafe extern "C" fn module_dispatch(state: u64, call: *mut $crate::abi::Call) -> i32
        {
            // The resident broker owns the generation-tagged ticket and buffers.
            unsafe { $crate::dispatch(&$driver, state, call) }
        }
    };
}

#[cfg(test)]
mod entry_tests
{
    use core::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    struct Probe
    {
        module: AtomicU64
    }
    impl Driver for Probe
    {
        fn initialize(&self, client: Client) -> Result<()>
        {
            self.module.store(client.module(), Ordering::Relaxed);
            Ok(())
        }

        fn request(&self, _: Request<'_>, _: &mut [u8]) -> Result<Completion>
        {
            Err(Error(NOT_SUPPORTED))
        }
    }
    unsafe extern "C" fn dispatch(_: u64, _: *mut abi::Call) -> Status
    {
        NOT_SUPPORTED
    }
    #[test]
    fn entry_preserves_full_module_incarnation_and_rejects_a_stale_host_layout_before_author_code()
    {
        let probe = Probe { module: AtomicU64::new(0) };
        let module = (1u64 << 47) | 0x1234;
        let mut host = abi::Host { header: abi::Header::of::<abi::Host>(),
                                   module,
                                   reserved: 0,
                                   context: module,
                                   call: None,
                                   resolve: None };
        let mut instance = abi::Instance::default();
        assert_eq!(unsafe { entry(&probe, &host, &mut instance, dispatch) }, OK);
        assert_eq!(probe.module.load(Ordering::Relaxed), module);
        probe.module.store(0, Ordering::Relaxed);
        host.header.size = 40;
        assert_eq!(unsafe { entry(&probe, &host, &mut instance, dispatch) }, BAD_ABI);
        assert_eq!(probe.module.load(Ordering::Relaxed),
                   0,
                   "stale descriptor reached module author initialization");
    }
    #[test]
    fn dependency_resolution_and_invocation_preserve_all_provider_bits()
    {
        const PROVIDER: u64 = (1 << 61) | 0x123456789;
        unsafe extern "C" fn resolve(_: u64, _: u32, _: u32, output: *mut u64) -> Status
        {
            unsafe {
                output.write(PROVIDER);
            }
            OK
        }
        unsafe extern "C" fn broker(_: u64, provider: u64, call: *mut abi::Call) -> Status
        {
            assert_eq!(provider, PROVIDER);
            let call = unsafe { &mut *call };
            assert_eq!(call.class, *b"TEST");
            call.output_len = 0;
            OK
        }
        let client = Client { host: abi::Host { header:   abi::Header::of::<abi::Host>(),
                                                module:   1 << 40,
                                                reserved: 0,
                                                context:  1 << 40,
                                                call:     Some(broker),
                                                resolve:  Some(resolve) } };
        let provider = client.resolve(ClassId(*b"TEST"), 1).unwrap();
        assert_eq!(provider.handle, PROVIDER);
        assert_eq!(provider.invoke(&mut 0, Phase::Start, 1, &[], &mut []).unwrap(),
                   Completion::Complete(0));
    }
}

#[cfg(test)]
mod synchronous_tests
{
    use core::sync::atomic::{AtomicUsize, Ordering};

    use mac_device_interface as wire;

    use super::*;
    use crate::abi;
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn broker(_: u64, _: u64, pointer: *mut abi::Call) -> i32
    {
        let call = unsafe { &mut *pointer };
        assert_eq!(call.phase, abi::SYNCHRONOUS);
        assert_eq!(call.ticket, 0);
        assert_eq!(call.class, wire::CLASS.0);
        assert_eq!(call.operation, wire::COUNT);
        assert_eq!(call.output_capacity, 4);
        // Contention cannot create a hidden request; a fresh retry remains
        // synchronous and has precisely the same bytes.
        if CALLS.fetch_add(1, Ordering::SeqCst) == 0
        {
            return mac_abi::SERVICE_BUSY;
        }
        unsafe {
            core::ptr::copy_nonoverlapping(32u32.to_le_bytes().as_ptr(), call.output, 4);
        }
        call.output_len = 4;
        mac_abi::OK
    }
    #[test]
    fn contention_retries_without_creating_or_discarding_a_request_ticket()
    {
        CALLS.store(0, Ordering::SeqCst);
        let service = Provider { client: Client { host: abi::Host { header:
                                                                        abi::Header::of::<abi::Host>(),
                                                                    module:   1 << 40,
                                                                    reserved: 0,
                                                                    context:  1 << 40,
                                                                    call:     Some(broker),
                                                                    resolve:  None } },
                                 handle: 1 << 41,
                                 class:  wire::CLASS };
        let mut output = [0; 4];
        assert_eq!(service.synchronous(wire::COUNT, &[], &mut output),
                   Err(Error(mac_abi::SERVICE_BUSY)));
        assert_eq!(service.synchronous(wire::COUNT, &[], &mut output), Ok(4));
        assert_eq!(u32::from_le_bytes(output), 32);
        assert_eq!(CALLS.load(Ordering::SeqCst), 2);
        assert!(!wire::synchronous(wire::RESET));
    }
}
