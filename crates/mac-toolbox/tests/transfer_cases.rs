//! Canonical guest transfer assertions. Only the public userspace SDK is used;
//! the host harness transports these outcomes without deciding their meaning.
#![cfg(target_arch = "riscv64")]
use mac_toolbox::{files::*, *};
fn require(value: bool) -> ToolboxResult<()>
{
    if value { Ok(()) } else { Err(Error::BadAbi) }
}
fn tick(system: &System<'_>) -> ToolboxResult<()>
{
    let event = system.events().wait_event(EventMask::ALL, 1)?;
    if let Event::FileTransferChanged { transfer,
                                        .. } = event
    {
        require(transfer.0 > u64::from(u32::MAX))?;
    }
    Ok(())
}
pub fn finish(system: &System<'_>, owner: &mut FileTransfer<'_>)
              -> core::result::Result<TransferProgress, TransferFailure>
{
    let mut recovery = None;
    let mut current = None;
    for _ in 0..10000
    {
        let progress = owner.step()?;
        current = progress.current;
        if progress.recovery != recovery
        {
            recovery = progress.recovery;
            if let Some(state) = recovery
            {
                crate::diagnostics::error(system, "TRANSFER_RECOVERY", state.error);
                system.log(if state.committed
                           {
                               "TRANSFER_RECOVERY_COMMITTED\n"
                           }
                           else
                           {
                               "TRANSFER_RECOVERY_ROLLBACK\n"
                           })
                      .ok();
                if let Some(entry) = state.current
                {
                    system.log(entry.name()).ok();
                    system.log("\n").ok();
                }
            }
        }
        if progress.finished()
        {
            return Ok(progress);
        }
        tick(system).map_err(|cause| {
                        TransferFailure { cause,
                                          cleanup: None,
                                          current: None }
                    })?;
    }
    Err(TransferFailure { cause: recovery.and_then(|state| state.original)
                                         .unwrap_or(Error::ServiceBusy),
                          cleanup: recovery.map(|state| state.error),
                          current })
}
fn success(system: &System<'_>, spec: TransferSpec, sources: &[TransferSource])
           -> ToolboxResult<()>
{
    let mut owner = system.files().begin_transfer(spec, sources)?;
    let outcome = finish(system, &mut owner).map_err(|failure| {
                      if let Some(entry) = failure.current
                      {
                          system.log(entry.name()).ok();
                          system.log("\n").ok();
                      }
                      crate::diagnostics::error(system, "TRANSFER_FAILED", failure.cause);
                      if let Some(cleanup) = failure.cleanup
                      {
                          crate::diagnostics::error(system, "TRANSFER_CLEANUP_FAILED", cleanup);
                      }
                      failure.cause
                  })?;
    require(outcome.phase == TransferPhase::Committed)
}
fn destination(volume: VolumeId, parent: Cnid, mode: TransferMode) -> TransferSpec
{
    TransferSpec { mode,
                   destination: TransferDestination::Folder { volume,
                                                              parent } }
}
fn reference(entry: &CatalogInfo) -> TransferSource
{
    TransferSource::new(entry.volume, entry.id, entry.parent)
}
fn file(system: &System<'_>, volume: VolumeId, parent: Cnid, name: &str, length: usize)
        -> ToolboxResult<CatalogInfo>
{
    let files = system.files();
    let mut base = CatalogInfo::EMPTY;
    base.file_type = TypeCode::new(*b"TEXT");
    base.creator = TypeCode::new(*b"TEST");
    base.created = Timestamp::from_mac_microseconds(3_875_171_696_123_456);
    base.modified = Timestamp::from_mac_microseconds(3_875_171_700_654_321);
    base.backup = Some(Timestamp::from_mac_microseconds(0));
    let metadata = ForkMetadata::from_catalog(&base, "Two forks café 🦀");
    let mut writer = files.begin_fork_write(ForkWriteSpec { volume,
                                                            parent,
                                                            name,
                                                            replace: None,
                                                            data_length: length as u64,
                                                            resource_length: 33,
                                                            metadata })?;
    let mut bytes = [0u8; 8192];
    let mut at = 0;
    while at < length
    {
        let n = (length - at).min(bytes.len());
        for (offset, byte) in bytes[..n].iter_mut().enumerate()
        {
            *byte = ((at + offset) * 17 + 3) as u8;
        }
        writer.write_chunk(Fork::Data, &bytes[..n])?;
        at += n;
    }
    for (at, byte) in bytes[..33].iter_mut().enumerate()
    {
        *byte = (at * 13 + 11) as u8;
    }
    writer.write_chunk(Fork::Resource, &bytes[..33])?;
    for _ in 0..10000
    {
        if let Some(entry) = writer.commit_step()?
        {
            return Ok(entry);
        }
        tick(system)?;
    }
    Err(Error::ServiceBusy)
}
fn bytes(system: &System<'_>, entry: &CatalogInfo) -> ToolboxResult<()>
{
    for (kind, length, factor, bias) in [(Fork::Data, entry.data_length, 17u64, 3u64),
                                         (Fork::Resource, entry.resource_length, 13, 11)]
    {
        let mut fork = system.files().open(entry.volume, entry.id, kind)?;
        let mut buffer = [0; 4096];
        let mut offset = 0;
        while offset < length
        {
            let n = fork.read(&mut buffer)?;
            require(n != 0 && n as u64 <= length - offset)?;
            for (at, &byte) in buffer[..n].iter().enumerate()
            {
                require(byte == ((offset + at as u64) * factor + bias) as u8)?;
            }
            offset += n as u64;
        }
        require(fork.read(&mut buffer)? == 0)?;
        fork.close()?;
    }
    Ok(())
}
fn properties(system: &System<'_>, source: &CatalogInfo, copied: &CatalogInfo)
              -> ToolboxResult<()>
{
    require(copied.name() == source.name()
            && copied.kind == source.kind
            && copied.data_length == source.data_length
            && copied.resource_length == source.resource_length
            && copied.created == source.created
            && copied.modified == source.modified
            && copied.backup == source.backup
            && copied.flags == source.flags
            && copied.finder_info == source.finder_info)?;
    let mut comment = Comment { bytes:  [0; 255],
                                length: 0 };
    system.files().comment(copied.volume, copied.id, &mut comment)?;
    require(comment.as_str() == "Two forks café 🦀")
}
fn failed(system: &System<'_>, spec: TransferSpec, sources: &[TransferSource], expected: Error)
          -> ToolboxResult<()>
{
    let mut owner = system.files().begin_transfer(spec, sources)?;
    let error = finish(system, &mut owner).err().ok_or(Error::BadAbi)?;
    require(error.cause == expected && error.cleanup.is_none() && owner.is_finished())?;
    crate::diagnostics::error(system, "TRANSFER_REJECTED", error.cause);
    Ok(())
}

/// Kill the private companion after real partial fork I/O. Cleanup ownership
/// remains resident; unrelated native storage and later transfers must work.
pub fn check_disconnect(system: &System<'_>, dead_source: bool) -> ToolboxResult<()>
{
    let files = system.files();
    let native = files.volume_at(0)?.id;
    let host = files.mount_host_export(0)?.id;
    let (source_volume, target_volume) = if dead_source { (host, native) } else { (native, host) };
    let source = file(system, source_volume, ROOT_ID, "Disconnected Source", 1024 * 1024)?;
    let target = files.new_folder(target_volume, ROOT_ID, "Disconnected Target")?;
    let mut transfer =
        files.begin_transfer(destination(target_volume, target.id, TransferMode::Copy),
                             &[reference(&source)])?;
    let mut injected = false;
    let mut failed = false;
    for _ in 0..10000
    {
        match transfer.step()
        {
            Ok(progress) =>
            {
                require(!progress.finished())?;
                if !injected && progress.completed_bytes > 0
                {
                    injected = true;
                    system.log("TRANSFER_DISCONNECT_READY\n")?;
                }
                if let Some(recovery) = progress.recovery
                {
                    let cause = recovery.original.ok_or(Error::BadAbi)?;
                    require(injected && matches!(cause, Error::Io | Error::RollbackFailed))?;
                    crate::diagnostics::error(system, "TRANSFER_DISCONNECT_FAILURE", cause);
                    crate::diagnostics::error(system, "TRANSFER_DISCONNECT_RECOVERY",
                                              recovery.error);
                    failed = true;
                    break;
                }
            }
            Err(error) =>
            {
                require(injected && matches!(error.cause, Error::Io | Error::RollbackFailed))?;
                crate::diagnostics::error(system, "TRANSFER_DISCONNECT_FAILURE", error.cause);
                failed = true;
                break;
            }
        }
        tick(system)?;
    }
    require(failed)?;
    drop(transfer);
    let mut retry = None;
    for _ in 0..1200
    {
        match files.new_folder(native, ROOT_ID, "Healthy Native Retry")
        {
            Ok(entry) =>
            {
                retry = Some(entry);
                break;
            }
            Err(Error::ServiceBusy) => tick(system)?,
            Err(error) => return Err(error)
        }
    }
    let retry = retry.ok_or(Error::ServiceBusy)?;
    if dead_source
    {
        require(files.lookup(native, target.id, source.name()) == Err(Error::NotFound))?;
    }
    else
    {
        bytes(system, &source)?;
    }
    let canary = file(system, native, ROOT_ID, "Healthy Transfer Source", 8193)?;
    success(system, destination(native, retry.id, TransferMode::Copy), &[reference(&canary)])?;
    bytes(system, &files.lookup(native, retry.id, canary.name())?)?;
    system.log("TRANSFER_DISCONNECT_PASS cleanup=owned native-storage=usable retry=passed\n")
}

pub fn check(system: &System<'_>, native: VolumeId, host: VolumeId, readonly: VolumeId,
             gate: mac_toolbox::abi::TrapGate)
             -> ToolboxResult<()>
{
    let files = system.files();
    system.log("FILE_TRANSFER_STAGE setup\n")?;
    let source = files.new_folder(native, ROOT_ID, "Transfer Source")?;
    let child = files.new_folder(native, source.id, "Child café")?;
    let data = file(system, native, child.id, "Document", 8199)?;
    let empty = file(system, native, source.id, "Empty data", 0)?;
    let source = files.catalog(native, source.id)?;
    let folder_metadata = ForkMetadata::from_catalog(&source, "Two forks café 🦀");
    files.set_properties(native, source.id, folder_metadata)?;
    let target = files.new_folder(host, ROOT_ID, "Transfer Target")?;
    let back = files.new_folder(native, ROOT_ID, "Transfer Return")?;
    system.log("FILE_TRANSFER_STAGE native-to-host\n")?;
    success(system, destination(host, target.id, TransferMode::Copy), &[reference(&source)])?;
    system.log("FILE_TRANSFER_STAGE copied\n")?;
    let copied = files.lookup(host, target.id, source.name())?;
    system.log("FILE_TRANSFER_STAGE copied-properties\n")?;
    properties(system, &source, &copied)?;
    system.log("FILE_TRANSFER_STAGE child-lookup\n")?;
    let copy_child = files.lookup(host, copied.id, child.name())?;
    system.log("FILE_TRANSFER_STAGE data-lookup\n")?;
    let copy_data = files.lookup(host, copy_child.id, data.name())?;
    properties(system, &data, &copy_data)?;
    bytes(system, &copy_data)?;
    system.log("FILE_TRANSFER_STAGE empty-lookup\n")?;
    let copy_empty = files.lookup(host, copied.id, empty.name())?;
    bytes(system, &copy_empty)?;
    require(files.catalog(native, source.id)?.id == source.id)?;
    // Both transfer directions use the same coordinator; deletion follows
    // verified destination publication rather than a Finder-side copy loop.
    system.log("FILE_TRANSFER_STAGE host-to-native\n")?;
    success(system, destination(native, back.id, TransferMode::Move), &[reference(&copied)])?;
    require(files.catalog(host, copied.id) == Err(Error::NotFound))?;
    let returned = files.lookup(native, back.id, source.name())?;
    properties(system, &source, &returned)?;
    let returned_child = files.lookup(native, returned.id, child.name())?;
    bytes(system, &files.lookup(native, returned_child.id, data.name())?)?;
    // Same-volume move keeps exact catalog identity.
    success(system, destination(native, back.id, TransferMode::Move), &[reference(&data)])?;
    require(files.lookup(native, back.id, data.name())?.id == data.id)?;
    // A same-folder forced copy receives a usable, noncolliding name.
    let data = files.catalog(native, data.id)?;
    success(system, destination(native, back.id, TransferMode::Copy), &[reference(&data)])?;
    let duplicate = files.lookup(native, back.id, "Document copy")?;
    require(duplicate.id != data.id)?;
    bytes(system, &duplicate)?;
    failed(system,
           destination(native, child.id, TransferMode::Copy),
           &[reference(&source)],
           Error::BadArgument)?;
    failed(system,
           destination(native, back.id, TransferMode::Copy),
           &[reference(&source)],
           Error::AlreadyExists)?;
    failed(system,
           destination(readonly, ROOT_ID, TransferMode::Copy),
           &[reference(&data)],
           Error::ReadOnly)?;
    let ro = files.lookup(readonly, ROOT_ID, "Host File.txt")?;
    failed(system,
           destination(native, back.id, TransferMode::Move),
           &[reference(&ro)],
           Error::ReadOnly)?;
    // Copying from immutable media is valid; refusal of Move grants no writes.
    success(system, destination(native, back.id, TransferMode::Copy), &[reference(&ro)])?;
    require(files.lookup(native, back.id, ro.name())?.data_length == ro.data_length)?;
    check_cancel(system, native, host, target.id)?;
    check_ids(system, native, host, target.id, &data)?;
    check_abandoned(system, native, host, target.id)?;
    check_trash(system, host)?;
    check_exit(system, native, host, gate)?;
    system.log("FILE_TRANSFER_PASS forks=passed recursive=passed directions=passed \
                identity=passed readonly=passed cancellation=passed stale=passed\n")
}
fn check_exit(system: &System<'_>, native: VolumeId, host: VolumeId,
              gate: mac_toolbox::abi::TrapGate)
              -> ToolboxResult<()>
{
    let files = system.files();
    let source = file(system, native, ROOT_ID, "Exit Transfer Source", 256 * 1024)?;
    let target = files.new_folder(host, ROOT_ID, "Exit Transfer Target")?;
    super::check_external_application(system, host, "Tools/Host Exit Worker", true, gate)?;
    let spec = destination(host, target.id, TransferMode::Copy);
    for _ in 0..10000
    {
        match files.begin_transfer(spec, &[reference(&source)])
        {
            Ok(mut owner) =>
            {
                require(files.lookup(host, target.id, source.name()) == Err(Error::NotFound))?;
                finish(system, &mut owner).map_err(|failure| failure.cause)?;
                bytes(system, &files.lookup(host, target.id, source.name())?)?;
                // Do not select the special worker branch on its later normal
                // open-fork/writer/watch owner-exit check.
                files.rename(native, source.id, "Exited Transfer Source")?;
                return system.log("FILE_TRANSFER_EXIT_PASS cleanup=passed retry=passed\n");
            }
            Err(Error::ServiceBusy) => tick(system)?,
            Err(error) => return Err(error)
        }
    }
    Err(Error::ServiceBusy)
}
fn check_abandoned(system: &System<'_>, native: VolumeId, host: VolumeId, target: Cnid)
                   -> ToolboxResult<()>
{
    let files = system.files();
    let source = file(system, native, ROOT_ID, "Abandoned Transfer", 128 * 1024)?;
    let spec = destination(host, target, TransferMode::Copy);
    let stale;
    {
        let mut owner = files.begin_transfer(spec, &[reference(&source)])?;
        stale = owner.id();
        for _ in 0..10000
        {
            let progress = owner.step().map_err(|failure| failure.cause)?;
            require(!progress.finished())?;
            if progress.completed_bytes != 0
            {
                break;
            }
            tick(system)?;
        }
        // Drop must retain the resident ledger until undo acknowledges; no
        // second client call is needed to drive cleanup.
    }
    use mac_toolbox::abi::files::{FileTransferCall, STEP_TRANSFER};
    let mut call = FileTransferCall { operation: STEP_TRANSFER,
                                      handle: stale.0,
                                      ..Default::default() };
    require(system.client().file_transfer(&mut call, &[]) == Err(Error::NotFound))?;
    let snapshot =
        system.inspection().snapshot(mac_toolbox::inspection::Scope::Own).map_err(|e| e.cause)?;
    let ledger = snapshot.ledger(0).map_err(|e| e.cause)?;
    let mut transfer_pool = None;
    for at in 0..ledger.len()
    {
        if ledger.at(at).map_err(|e| e.cause)?.kind == *b"FTXN"
        {
            transfer_pool = Some(at);
            break;
        }
    }
    let transfer_pool = transfer_pool.ok_or(Error::BadAbi)?;
    for _ in 0..10000
    {
        // Invalidating an ID is not an undo receipt. Observe the same public
        // ledger that holds the process death barrier until actual completion.
        let snapshot = system.inspection()
                             .snapshot(mac_toolbox::inspection::Scope::Own)
                             .map_err(|e| e.cause)?;
        let pool =
            snapshot.ledger(0).map_err(|e| e.cause)?.at(transfer_pool).map_err(|e| e.cause)?;
        if pool.pending == 0
        {
            require(files.lookup(host, target, source.name()) == Err(Error::NotFound))?;
            bytes(system, &source)?;
            let mut retry = files.begin_transfer(spec, &[reference(&source)])?;
            require(retry.id() != stale
                    && retry.id().0 > u64::from(u32::MAX)
                    && stale.0 > u64::from(u32::MAX))?;
            require(system.client().file_transfer(&mut call, &[]) == Err(Error::NotFound))?;
            let progress = finish(system, &mut retry).map_err(|failure| failure.cause)?;
            require(progress.phase == TransferPhase::Committed)?;
            return system.log("FILE_TRANSFER_DROP_PASS cleanup=passed retry=passed \
                               ledger=passed\n");
        }
        tick(system)?;
    }
    Err(Error::ServiceBusy)
}
fn check_trash(system: &System<'_>, volume: VolumeId) -> ToolboxResult<()>
{
    let files = system.files();
    let folder = files.new_folder(volume, ROOT_ID, "Recursive Trash")?;
    let child = file(system, volume, folder.id, "Trashed document", 8199)?;
    files.move_to_trash(volume, folder.id)?;
    let trash = files.trash_directory(volume, false)?;
    require(files.catalog(volume, folder.id)?.parent == trash.id)?;
    let usage = files.trash_usage(volume)?;
    require(usage.items == 2 && usage.allocated_bytes.is_none())?;
    files.put_away(volume, folder.id)?;
    require(files.catalog(volume, folder.id)?.parent == ROOT_ID)?;
    require(files.trash_usage(volume)?.items == 0)?;
    files.move_to_trash(volume, folder.id)?;
    files.empty_trash(volume)?;
    require(files.catalog(volume, folder.id) == Err(Error::NotFound)
            && files.catalog(volume, child.id) == Err(Error::NotFound)
            && files.trash_usage(volume)?.items == 0)?;
    files.empty_trash(volume)?;
    system.log("FILE_TRANSFER_TRASH_PASS recursive=passed put-away=passed empty=passed \
                retry=passed\n")
}
fn check_cancel(system: &System<'_>, native: VolumeId, host: VolumeId, target: Cnid)
                -> ToolboxResult<()>
{
    let files = system.files();
    let folder = files.new_folder(native, ROOT_ID, "Cancel Transfer")?;
    let large = file(system, native, folder.id, "Large", 512 * 1024)?;
    let before = files.catalog(native, folder.id)?;
    let target_before = files.catalog(host, target)?;
    let native_date = files.volume(native)?.modified;
    let host_date = files.volume(host)?.modified;
    let spec = destination(host, target, TransferMode::Move);
    let mut owner = files.begin_transfer(spec, &[reference(&folder)])?;
    let mut stopping = false;
    for _ in 0..10000
    {
        let progress =
            if stopping { owner.cancel_step() } else { owner.step() }.map_err(|failure| {
                                                                         failure.cause
                                                                     })?;
        if progress.phase == TransferPhase::Committed
        {
            return Err(Error::BadAbi);
        }
        if progress.phase == TransferPhase::Cancelled
        {
            require(stopping)?;
            require(files.catalog(native, folder.id)? == before
                    && files.catalog(native, large.id)?.id == large.id)?;
            require(files.lookup(host, target, folder.name()) == Err(Error::NotFound))?;
            require(files.catalog(host, target)?.modified == target_before.modified)?;
            require(files.volume(native)?.modified == native_date
                    && files.volume(host)?.modified == host_date)?;
            bytes(system, &large)?;
            // A new operation is admitted immediately after undo, using the
            // same source and target that the failed drag would retry.
            success(system, destination(host, target, TransferMode::Copy), &[reference(&folder)])?;
            return Ok(());
        }
        if progress.completed_bytes != 0
        {
            stopping = true;
        }
        tick(system)?;
    }
    Err(Error::ServiceBusy)
}
fn check_ids(system: &System<'_>, native: VolumeId, host: VolumeId, target: Cnid,
             data: &CatalogInfo)
             -> ToolboxResult<()>
{
    use mac_toolbox::abi::files::{FileTransferCall, STEP_TRANSFER};
    let spec = destination(host, target, TransferMode::Copy);
    let mut old = system.files().begin_transfer(spec, &[reference(data)])?;
    let stale = old.id();
    // This succeeds, then a duplicate request reuses its resident slot and
    // fails normally. The old generation must never observe that replacement.
    finish(system, &mut old).map_err(|failure| failure.cause)?;
    let mut replacement = system.files().begin_transfer(spec, &[reference(data)])?;
    require(replacement.id() != stale
            && stale.0 > u64::from(u32::MAX)
            && replacement.id().0 > u64::from(u32::MAX))?;
    let mut call = FileTransferCall { operation: STEP_TRANSFER,
                                      handle: stale.0,
                                      ..Default::default() };
    require(system.client().file_transfer(&mut call, &[]) == Err(Error::NotFound))?;
    let failure = finish(system, &mut replacement).err().ok_or(Error::BadAbi)?;
    require(failure.cause == Error::AlreadyExists)?;
    use mac_toolbox::abi::files::{CLOSE, FileCall, OPEN};
    let mut opened = FileCall { operation: OPEN,
                                volume: native.0,
                                cnid: data.id.0,
                                ..Default::default() };
    system.client().file(&mut opened, "", &mut [])?;
    let mut foreign = FileTransferCall { operation: STEP_TRANSFER,
                                         handle: u64::from(opened.handle),
                                         ..Default::default() };
    require(system.client().file_transfer(&mut foreign, &[]) == Err(Error::NotFound))?;
    system.client().file(&mut FileCall { operation: CLOSE,
                                          handle: opened.handle,
                                          ..Default::default() },
                          "",
                          &mut [])?;
    Ok(())
}

struct Comment
{
    bytes:  [u8; 255],
    length: usize
}
impl Comment
{
    fn as_str(&self) -> &str
    {
        core::str::from_utf8(&self.bytes[..self.length]).unwrap()
    }
}
impl core::fmt::Write for Comment
{
    fn write_str(&mut self, value: &str) -> core::fmt::Result
    {
        let end = self.length
                      .checked_add(value.len())
                      .filter(|&end| end <= self.bytes.len())
                      .ok_or(core::fmt::Error)?;
        self.bytes[self.length..end].copy_from_slice(value.as_bytes());
        self.length = end;
        Ok(())
    }
}
