//! Raw ownership transport used by the platform GlobalAlloc implementation.
//! Ordinary applications allocate through Rust collections; this module does
//! not expose a safe arbitrary-pointer disposal operation.
use core::{alloc::Layout, fmt, ptr::NonNull};

use mac_abi::{Status, application_heap as wire};

use crate::{Client, Error, types::InlineText};
#[derive(Clone, Copy, Debug)]
pub enum Failure
{
    Resident
    {
        cause: Error, detail: InlineText<{ wire::DETAIL_BYTES }>
    },
    Protocol
    {
        returned_status: i64, detail: &'static str
    }
}
impl Failure
{
    pub fn cause(self) -> Error
    {
        match self
        {
            Self::Resident { cause,
                             .. } => cause,
            Self::Protocol { .. } => Error::BadAbi
        }
    }

    pub fn is_exhaustion(&self) -> bool
    {
        matches!(self,
                 Self::Resident { cause: Error::NoMemory,
                                  .. })
    }
}
impl fmt::Display for Failure
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Resident { cause,
                             detail } => write!(f, "{cause} ({}): {detail}", cause.status()),
            Self::Protocol { returned_status,
                             detail } =>
            {
                write!(f,
                       "application heap protocol failure: {detail}; returned \
                        status={returned_status}")
            }
        }
    }
}
impl core::error::Error for Failure
{
}
pub type Result<T> = core::result::Result<T, Failure>;
fn exchange(client: Client<'_>, call: &mut wire::Call) -> Result<()>
{
    let original = (call.operation, call.pointer, call.old_size, call.alignment, call.new_size);
    let status = client.invoke(mac_abi::traps::APPLICATION_HEAP, call);
    let invalid = |detail| {
        Failure::Protocol { returned_status: status,
                            detail }
    };
    if call.operation != original.0
       || call.reserved != 0
       || call.reserved2 != 0
       || call.pointer != original.1
       || call.old_size != original.2
       || call.alignment != original.3
       || call.new_size != original.4
    {
        return Err(invalid("resident changed allocator input fields"));
    }
    let status: Status =
        status.try_into().map_err(|_| invalid("status exceeds native Status width"))?;
    if status > 0
    {
        return Err(invalid("positive status is not an allocator result"));
    }
    if status == mac_abi::OK
    {
        if call.detail_length != 0
        {
            return Err(invalid("success returned failure detail"));
        }
        if call.operation == wire::DEALLOCATE
        {
            if !call.result.is_null()
            {
                return Err(invalid("disposal returned an allocation"));
            }
        }
        else if call.result.is_null()
                  || !call.result.addr().is_multiple_of(call.alignment)
                  || call.result.addr().checked_add(call.new_size).is_none()
        {
            return Err(invalid("successful allocation has a null, misaligned or overflowing \
                                extent"));
        }
        return Ok(());
    }
    if !call.result.is_null()
    {
        return Err(invalid("failure published an allocation"));
    }
    let detail =
        InlineText::from_reply(call.detail, call.detail_length).map_err(|_| {
                                                                   invalid("failure detail is not \
                                                                            bounded UTF-8")
                                                               })?;
    if detail.is_empty()
    {
        return Err(invalid("failure omitted its diagnostic cause"));
    }
    Err(Failure::Resident { cause: Error::from_status(status),
                            detail })
}
impl Client<'_>
{
    /// # Safety
    /// Nonzero Layout is a valid Rust allocation request. On success the caller
    /// owns the returned storage and must eventually dispose it once using the
    /// same application heap and exact layout. This does not mint a handle ID.
    pub unsafe fn allocate_application(self, layout: Layout, zeroed: bool) -> Result<NonNull<u8>>
    {
        let mut call =
            wire::Call { operation: if zeroed { wire::ALLOCATE_ZEROED } else { wire::ALLOCATE },
                         alignment: layout.align(),
                         new_size: layout.size(),
                         ..Default::default() };
        exchange(self, &mut call)?;
        NonNull::new(call.result).ok_or(Failure::Protocol { returned_status: 0,
                                                            detail:          "successful \
                                                                              allocation lost its \
                                                                              result" })
    }

    /// # Safety
    /// Pointer is one live allocation from this application's heap, has its
    /// exact original nonzero Layout, and no reader or alias survives disposal.
    pub unsafe fn deallocate_application(self, pointer: NonNull<u8>, layout: Layout) -> Result<()>
    {
        let mut call = wire::Call { operation: wire::DEALLOCATE,
                                    pointer: pointer.as_ptr(),
                                    old_size: layout.size(),
                                    alignment: layout.align(),
                                    ..Default::default() };
        exchange(self, &mut call)
    }

    /// # Safety
    /// Pointer/old Layout identify one exclusively owned live allocation from
    /// this application's heap. New size is nonzero and Layout-valid at the
    /// same alignment. Failure preserves ownership/layout/data of the old block;
    /// success consumes it and transfers the returned allocation to the caller.
    pub unsafe fn reallocate_application(self, pointer: NonNull<u8>, layout: Layout,
                                         new_size: usize)
                                         -> Result<NonNull<u8>>
    {
        let mut call = wire::Call { operation: wire::REALLOCATE,
                                    pointer: pointer.as_ptr(),
                                    old_size: layout.size(),
                                    alignment: layout.align(),
                                    new_size,
                                    ..Default::default() };
        exchange(self, &mut call)?;
        NonNull::new(call.result).ok_or(Failure::Protocol { returned_status: 0,
                                                            detail:          "successful \
                                                                              reallocation lost \
                                                                              its result" })
    }
}
