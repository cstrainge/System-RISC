use crate::sys::pal::runtime;

pub fn fill_bytes(bytes: &mut [u8])
{
    if bytes.is_empty()
    {
        return;
    }
    let status = unsafe { runtime::__system_risc_entropy_v1(bytes.as_mut_ptr(), bytes.len()) };
    if status != mac_abi::OK
    {
        runtime::failure(status, b"System entropy service could not fill the requested buffer");
    }
}
