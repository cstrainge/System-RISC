//! File-backed mount publication uses the normal FSYS callback path. A backing
//! lease survives provider failure/cancellation until Unmount acknowledges it.
#![forbid(unsafe_code)]
use super::*;
use crate::drivers::file_sources::runtime as backing;
pub(super) fn start_file(owner: u64, thread: u64, key: Key, call: &mut FileCall, output: &mut [u8])
                         -> Status
{
    let provider = match crate::drivers::find_kernel_provider(ClassId(fs::CLASS),
                                                              fs::VERSION,
                                                              fs::SOURCE_FILE)
    {
        Ok(provider) => provider,
        Err(error) => return error
    };
    let (source, fresh) = match backing::lease(provider, call.volume, call.cnid)
    {
        Ok(lease) => lease,
        Err(error) => return error
    };
    let identity = super::super::Source::File(source);
    match files(|manager| manager.existing_provider_mount(provider, identity))
    {
        Ok(Some(info)) => return reply(call, output, Ok(Reply::Mounted(info))),
        Ok(None) => (),
        Err(error) =>
        {
            if fresh
            {
                let _ = backing::release(provider, source);
            }
            return error;
        }
    }
    let volume = match files(|manager| manager.reserve_provider_mount(provider, identity))
    {
        Ok(volume) => volume,
        Err(error) =>
        {
            if fresh
            {
                let _ = backing::release(provider, source);
            }
            return error;
        }
    };
    let job = match submit(provider, Operation::Mount, volume, u64::from(source))
    {
        Ok(job) => job,
        Err(error) =>
        {
            return retire(volume).map_or_else(|cleanup| cleanup, |_| error);
        }
    };
    waiting(Flight { thread,
                     owner,
                     key,
                     provider,
                     step: Step::Mount(volume),
                     job,
                     abandoned: false,
                     input: String::new(),
                     path: None })
}
