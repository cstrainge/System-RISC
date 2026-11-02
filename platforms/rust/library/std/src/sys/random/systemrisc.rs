use crate::sys::pal::{failure, runtime};

pub fn fill_bytes(bytes: &mut [u8])
{
    if bytes.is_empty()
    {
        return;
    }
    if let Err(cause) = runtime::client().entropy_fill(bytes)
    {
        failure::report(format_args!("System entropy required by std failed: {cause}"));
    }
}
