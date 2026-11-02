use crate::sys::pal::runtime;

pub type Key = usize;

pub fn create(destructor: Option<unsafe extern "C" fn(*mut u8)>) -> Key
{
    unsafe { runtime::__system_risc_tls_create_v1(destructor) }
}
pub unsafe fn get(key: Key) -> *mut u8
{
    unsafe { runtime::__system_risc_tls_get_v1(key) }
}
pub unsafe fn set(key: Key, value: *mut u8)
{
    unsafe { runtime::__system_risc_tls_set_v1(key, value) }
}
pub unsafe fn destroy(key: Key)
{
    unsafe { runtime::__system_risc_tls_delete_v1(key) }
}
