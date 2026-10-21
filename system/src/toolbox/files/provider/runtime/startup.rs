//! Device discovery mounts exports before userspace starts. Use the same owned
//! jobs, publication validation and acknowledged cleanup as explicit mounts.
#![forbid(unsafe_code)]
use super::*;

// Retain delivery metadata inline: mounting must not need a second allocation
// after publication merely to report the already-mounted identity.
#[allow(clippy::large_enum_variant)]
pub(super) enum HostMount
{
    Existing(VolumeInfo),
    Pending(u32, OwnedJob)
}

pub(super) fn host_mount(provider: u32, index: u32) -> Result<HostMount, Status>
{
    let source = super::super::Source::Export(u64::from(index));
    if let Some(info) = files(|manager| manager.existing_provider_mount(provider, source))?
    {
        return Ok(HostMount::Existing(info));
    }
    let volume = files(|manager| manager.reserve_provider_mount(provider, source))?;
    match submit(provider, Operation::Mount, volume, u64::from(index))
    {
        Ok(job) => Ok(HostMount::Pending(volume, job)),
        Err(error) => retire_unstarted_export(volume).and(Err(error))
    }
}

fn complete(provider: u32, step: Step, job: OwnedJob) -> Result<Reply, Status>
{
    // Bootstrap has no application pointer or parked userspace caller. A single
    // reserved flight slot retains cancellation/cleanup if startup times out.
    let mut flight = Flight { thread: crate::context::BOOT_THREAD.0,
                              owner: 0,
                              key: Key { record:    Record::File,
                                         operation: 0,
                                         volume:    0,
                                         index:     0,
                                         cnid:      0,
                                         handle:    0,
                                         fork:      0,
                                         capacity:  0,
                                         cursor:    DirectoryScanCursor::default(),
                                         resource:  None },
                              provider,
                              step,
                              job,
                              abandoned: false,
                              input: String::new(),
                              path: None };
    let start = crate::platform::ticks();
    loop
    {
        crate::drivers::pump_jobs();
        flight = match advance(flight)
        {
            Progress::Done(result) => return result,
            Progress::Pending(flight) => flight
        };
        if crate::platform::ticks().wrapping_sub(start) >= 50_000_000
        {
            flight.abandon();
            FLIGHTS.lock().push(flight);
            REAP.store(true, Ordering::Release);
            return Err(IO_ERROR);
        }
        core::hint::spin_loop();
    }
}

pub(crate) fn mount_startup_exports() -> Result<(), Status>
{
    if crate::context::current_thread() != crate::context::BOOT_THREAD
    {
        return Err(WRONG_THREAD);
    }
    let provider = match crate::drivers::find_kernel_provider(ClassId(fs::CLASS),
                                                              fs::VERSION,
                                                              fs::SOURCE_HOST_EXPORT)
    {
        Ok(provider) => provider,
        Err(NOT_FOUND) => return Ok(()),
        Err(error) => return Err(error)
    };
    // No callbacks or jobs exist until their cleanup storage is reserved.
    FLIGHTS.lock().try_reserve(1).map_err(|_| NO_MEMORY)?;
    let job = submit(provider, Operation::Exports, 0, 0)?;
    let Reply::Count(count) = complete(provider, Step::Count, job)?
    else
    {
        return Err(BAD_ABI);
    };
    if count > 16
    {
        return Err(BAD_ABI);
    }
    for index in 0..count
    {
        let result =
            host_mount(provider, index).and_then(|mount| {
                                           match mount
                                           {
                                               HostMount::Existing(info) =>
                                               {
                                                   Ok(Reply::Mounted(info))
                                               }
                                               HostMount::Pending(volume, job) =>
                                               {
                                                   complete(provider, Step::Mount(volume), job)
                                               }
                                           }
                                       });
        match result
        {
            Ok(Reply::Mounted(info)) =>
            {
                serial!("HOST_VOLUME_MOUNTED index={} id={} read_only={}", index, info.id,
                        info.read_only)
            }
            Ok(_) => return Err(BAD_ABI),
            Err(error) =>
            {
                serial!("HOST_VOLUME_MOUNT_FAILED index={} status={}", index, error);
                // A timeout retained a cancellation flight. Do not queue more
                // startup requests behind its still-live provider operation.
                if REAP.load(Ordering::Acquire)
                {
                    return Err(error);
                }
            }
        }
    }
    Ok(())
}
