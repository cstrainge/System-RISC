use mac_traps::application_heap::Failure;

use crate::{alloc::Layout,
            fmt::{self, Write},
            ptr::{self, NonNull},
            sys::pal::runtime};

fn failed(cause: Failure) -> !
{
    // Error formatting must not allocate or touch std TLS/stdio locks.
    struct Message
    {
        bytes: [u8; 512],
        used:  usize
    }
    impl Write for Message
    {
        fn write_str(&mut self, text: &str) -> fmt::Result
        {
            let mut count = text.len().min(self.bytes.len() - self.used);
            while !text.is_char_boundary(count)
            {
                count -= 1;
            }
            self.bytes[self.used..self.used + count].copy_from_slice(&text.as_bytes()[..count]);
            self.used += count;
            if count == text.len() { Ok(()) } else { Err(fmt::Error) }
        }
    }
    let mut message = Message { bytes: [0; 512],
                                used:  0 };
    let _ = write!(message, "application allocator: {cause}");
    runtime::failure(cause.cause().status(), &message.bytes[..message.used])
}

pub unsafe fn alloc(layout: Layout) -> *mut u8
{
    match unsafe { runtime::client().allocate_application(layout, false) }
    {
        Ok(pointer) => pointer.as_ptr(),
        Err(cause) if cause.is_exhaustion() => ptr::null_mut(),
        Err(cause) => failed(cause)
    }
}

pub unsafe fn alloc_zeroed(layout: Layout) -> *mut u8
{
    match unsafe { runtime::client().allocate_application(layout, true) }
    {
        Ok(pointer) => pointer.as_ptr(),
        Err(cause) if cause.is_exhaustion() => ptr::null_mut(),
        Err(cause) => failed(cause)
    }
}

pub unsafe fn dealloc(pointer: *mut u8, layout: Layout)
{
    // GlobalAlloc callers guarantee a nonnull pointer and its original layout.
    if let Err(cause) =
        unsafe { runtime::client().deallocate_application(NonNull::new_unchecked(pointer), layout) }
    {
        failed(cause);
    }
}

pub unsafe fn realloc(pointer: *mut u8, layout: Layout, size: usize) -> *mut u8
{
    match unsafe {
              runtime::client().reallocate_application(NonNull::new_unchecked(pointer),
                                                       layout,
                                                       size)
          }
    {
        Ok(pointer) => pointer.as_ptr(),
        Err(cause) if cause.is_exhaustion() => ptr::null_mut(),
        Err(cause) => failed(cause)
    }
}
