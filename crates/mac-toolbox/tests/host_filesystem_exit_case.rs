//! A real second application deliberately exits with two live provider marks.
use mac_toolbox::{Error, System,
                  files::{Fork, ROOT_ID}};
fn require(value: bool) -> Result<(), Error>
{
    if value { Ok(()) } else { Err(Error::BadAbi) }
}
// Failure-only native diagnostics. No extra traps or output on successful
// reads/retries; the original refusal remains the caller's result.
fn report(system: &System<'_>, args: core::fmt::Arguments<'_>)
{
    use core::fmt::Write;
    struct Buffer
    {
        bytes:  [u8; 1024],
        length: usize
    }
    impl core::fmt::Write for Buffer
    {
        fn write_str(&mut self, text: &str) -> core::fmt::Result
        {
            if text.len() > self.bytes.len() - self.length - 1
            {
                return Err(core::fmt::Error);
            }
            self.bytes[self.length..self.length + text.len()].copy_from_slice(text.as_bytes());
            self.length += text.len();
            Ok(())
        }
    }
    let mut buffer = Buffer { bytes:  [0; 1024],
                              length: 0 };
    let truncated = buffer.write_fmt(args).is_err();
    buffer.bytes[buffer.length] = b'\n';
    buffer.length += 1;
    if let Ok(text) = core::str::from_utf8(&buffer.bytes[..buffer.length])
    {
        let _ = system.log(text);
    }
    if truncated
    {
        let _ = system.log("HOST_FS_EXIT_DETAIL_TRUNCATED\n");
    }
}

fn check_transfer_ledger(system: &System<'_>) -> Result<(), Error>
{
    use mac_toolbox::inspection::Scope;
    // Same 64 coherent-snapshot attempts as inspection_read::own. Keep the
    // final public Failure, rather than losing its detail when taking cause.
    for attempt in 1..=64
    {
        let mut phase = "snapshot";
        let mut membership = None;
        let mut ledger_revision = None;
        let mut row = None;
        let mut detail = None;
        let result = (|| {
            let snapshot = system.inspection().snapshot(Scope::Own).map_err(|failure| {
                                                                        detail = Some(failure);
                                                                        failure.cause
                                                                    })?;
            membership = Some(snapshot.revision);
            phase = "ledger";
            let ledger = snapshot.ledger(0).map_err(|failure| {
                                                detail = Some(failure);
                                                failure.cause
                                            })?;
            ledger_revision = Some(ledger.domain().ledger_revision);
            let mut found = false;
            let mut scoped = 0u64;
            let mut owned_io = 0u32;
            let mut jobs_seen = false;
            let mut requests_seen = false;
            for index in 0..ledger.len()
            {
                row = Some(index);
                phase = "pool";
                let pool = ledger.at(index).map_err(|failure| {
                                                detail = Some(failure);
                                                failure.cause
                                            })?;
                phase = "pool-assertion";
                scoped += pool.scoped_children;
                if [*b"FFRK", *b"FSWR"].contains(&pool.kind)
                {
                    require(pool.live > 0 && pool.scoped_children == pool.live + pool.pending)?;
                    owned_io += 1;
                }
                if pool.kind == *b"DJOB" || pool.kind == *b"DREQ"
                {
                    require(pool.scoped_children == pool.live + pool.pending)?;
                    if pool.kind == *b"DJOB"
                    {
                        jobs_seen = true;
                    }
                    if pool.kind == *b"DREQ"
                    {
                        requests_seen = true;
                    }
                }
                if pool.kind == *b"FTXN"
                {
                    require(pool.live == 1 && pool.pending == 0)?;
                    found = true;
                }
            }
            phase = "ledger-assertion";
            row = None;
            require(found
                    && jobs_seen
                    && requests_seen
                    && owned_io == 2
                    && scoped >= 2
                    && ledger.domain().scoped_children == scoped)
        })();
        match result
        {
            Err(Error::ScanStale) if attempt < 64 => (),
            Err(cause) =>
            {
                report(system,
                       format_args!("HOST_FS_EXIT_DETAIL stage=transfer-ledger phase={} \
                                     attempt={} membership={:?} ledger_revision={:?} \
                                     pool_row={:?} status={} detail={}",
                                    phase,
                                    attempt,
                                    membership,
                                    ledger_revision,
                                    row,
                                    cause.status(),
                                    detail.as_ref()
                                          .map(|failure| failure as &dyn core::fmt::Display)
                                          .unwrap_or(&cause)));
                return Err(cause);
            }
            Ok(()) => return Ok(())
        }
    }
    Err(Error::ScanStale)
}

pub fn check(system: &System<'_>) -> Result<(), Error>
{
    let files = system.files();
    let own = system.processes().current()?;
    let own_volume = files.volume(own.volume)?;
    require(!own_volume.startup && own.name() == "Host Exit Worker")?;
    let mut text = [0; 64];
    require(system.resources().application().string(2000, &mut text)?
            == "external-application-resource")?;
    system.log(if own_volume.read_only
               {
                   "HOST_FS_EXTERNAL_APPLICATION_READY_RO\n"
               }
               else
               {
                   "HOST_FS_EXTERNAL_APPLICATION_READY_RW\n"
               })?;
    let mut released = false;
    for _ in 0..300
    {
        if matches!(system.events().wait_event(mac_toolbox::EventMask::KEY_DOWN, 1)?,
                    mac_toolbox::Event::KeyDown { .. })
        {
            released = true;
            break;
        }
    }
    require(released)?;
    if own_volume.read_only
    {
        system.log("HOST_FS_EXTERNAL_APPLICATION_EXIT_RO\n")?;
        return Ok(());
    }
    let mut mounted = None;
    for index in 0..files.volume_count()?
    {
        let info = files.volume_at(index)?;
        if info.name() == "Native Host"
        {
            mounted = Some(info.id);
            break;
        }
    }
    let volume = mounted.ok_or(Error::NotFound)?;
    let startup = files.volume_at(0)?.id;
    if let Ok(source) = files.lookup(startup, ROOT_ID, "Exit Transfer Source")
    {
        use mac_toolbox::files::*;
        let target = files.lookup(volume, ROOT_ID, "Exit Transfer Target").map_err(|cause| {
            report(system, format_args!("HOST_FS_EXIT_DETAIL stage=target-lookup status={} detail={}", cause.status(), cause));
            cause
        })?;
        let mut owner =
            files.begin_transfer(TransferSpec { mode:        TransferMode::Copy,
                                                destination:
                                                    TransferDestination::Folder { volume,
                                                                                  parent:
                                                                                      target.id } },
                                 &[TransferSource::new(startup, source.id, ROOT_ID)])
                 .map_err(|cause| {
                     report(system,
                            format_args!("HOST_FS_EXIT_DETAIL stage=begin-transfer status={} \
                                          detail={}",
                                         cause.status(),
                                         cause));
                     cause
                 })?;
        for _ in 0..10000
        {
            let progress = owner.step().map_err(|failure| {
                                            report(system,
                                                   format_args!("HOST_FS_EXIT_DETAIL \
                                                                 stage=transfer-step status={} \
                                                                 detail={}",
                                                                failure.cause.status(),
                                                                failure));
                                            failure.cause
                                        })?;
            require(!progress.finished())?;
            if progress.completed_bytes != 0
            {
                check_transfer_ledger(system)?;
                system.log("HOST_FS_TRANSFER_SCOPES_PASS owner=app fork=owned writer=owned\n")?;
                system.log("HOST_FS_TRANSFER_LEDGER_PASS\n")?;
                core::mem::forget(owner);
                system.log("HOST_FS_EXIT_WORKER_READY\n")?;
                return Ok(());
            }
            system.events().wait_event(mac_toolbox::EventMask::ALL, 1)?;
        }
        return Err(Error::ServiceBusy);
    }
    let folder = files.lookup(volume, ROOT_ID, "Tools")?;
    let file = files.lookup(volume, folder.id, "Forks")?;
    let mut data = files.open(volume, file.id, Fork::Data)?;
    let mut resources = files.open(volume, file.id, Fork::Resource)?;
    let mut bytes = [0; 3];
    require(data.read(&mut bytes)? == 3 && bytes == [7, 38, 69])?;
    require(resources.read(&mut bytes)? == 3 && bytes == [11, 24, 37])?;
    require(files.unmount(volume) == Err(Error::ServiceBusy))?;
    let mut writer =
        files.create_data(mac_toolbox::files::DataFileSpec { volume,
                                                             parent: ROOT_ID,
                                                             name: "Exit staging",
                                                             replacing: None,
                                                             file_type: (*b"TEXT").into(),
                                                             creator: (*b"TEST").into() },
                          3)?;
    writer.write_all(b"abc")?;
    let mut metadata = mac_toolbox::files::CatalogInfo::EMPTY;
    metadata.created = mac_toolbox::Timestamp::from_mac_microseconds(3_875_171_696_123_456);
    metadata.modified = metadata.created;
    let mut streamed = files.begin_fork_write(mac_toolbox::files::ForkWriteSpec {
        volume, parent: ROOT_ID, name: "Exit fork staging", replace: None,
        data_length: 3, resource_length: 0,
        metadata: mac_toolbox::files::ForkMetadata::from_catalog(&metadata, "")
    })?;
    streamed.write_chunk(Fork::Data, b"ack")?;
    core::mem::forget(streamed);
    core::mem::forget(writer);
    core::mem::forget(files.watch_directory(volume, ROOT_ID)?);
    core::mem::forget(data);
    core::mem::forget(resources);
    system.log("HOST_FS_EXIT_WORKER_READY\n")?;
    // Returning enters the real scheduler/process EXIT path, bypassing Drop.
    Ok(())
}
