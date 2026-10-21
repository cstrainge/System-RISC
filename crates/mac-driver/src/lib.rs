//! Native in-kernel module SDK. Ordinary driver logic uses borrowed Rust slices;
//! this module is the narrow, audited C/pointer boundary. This is not an app SDK.
#![no_std]
pub mod devices;
pub mod file_sources;
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
    pub ticket:    u32,
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
    pub fn module(self) -> u32
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
    handle: u32,
    class:  ClassId
}
impl Provider
{
    /// A provider handle is issued by the resident registry, never an address.
    /// The resident validates the declaring module's dependency/service on every
    /// call. Pending and pre-dispatch contention retain a live ticket; terminal
    /// completion clears it. Never discard a pending ticket before cancellation
    /// is acknowledged.
    pub fn invoke(self, ticket: &mut u32, phase: Phase, operation: u32, input: &[u8],
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
